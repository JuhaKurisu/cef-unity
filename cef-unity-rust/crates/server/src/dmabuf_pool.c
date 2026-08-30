// Linux: CEF の on_accelerated_paint が渡す dmabuf はプールの借用で、
// コールバックを抜けると再利用される。そのため自前の出力バッファへ blit し、
// その dmabuf をクライアントへ渡す。macOS の iosurface_pool.m に相当する。
//
// EGL コンテキストは作成したスレッドでしか current にできない。このプールは
// on_accelerated_paint が来るスレッドで生成し、同じスレッドから使うこと。

#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>
#include <fcntl.h>
#include <gbm.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

typedef struct {
    int drm_file_descriptor;
    struct gbm_device *gbm_device;
    EGLDisplay display;
    EGLContext context;

    struct gbm_bo *output_buffer_object;
    EGLImageKHR output_image;
    GLuint output_texture;
    GLuint output_framebuffer;
    int output_width;
    int output_height;
    unsigned int generation;

    PFNEGLCREATEIMAGEKHRPROC create_image;
    PFNEGLDESTROYIMAGEKHRPROC destroy_image;
    PFNGLEGLIMAGETARGETTEXTURE2DOESPROC image_target_texture;
} DmabufPool;

void dmabuf_pool_destroy(void *handle);

// 構築が失敗した段階。呼び出し側 (Rust) がログに出して原因を切り分けるために返す。
// 環境不足で失敗するのは想定内なので、失敗自体はエラーではない (software paint に落ちる)。
#define DMABUF_POOL_STAGE_SUCCESS 0
#define DMABUF_POOL_STAGE_OPEN_RENDER_NODE 1
#define DMABUF_POOL_STAGE_CREATE_GBM_DEVICE 2
#define DMABUF_POOL_STAGE_GET_PLATFORM_DISPLAY_PROC 3
#define DMABUF_POOL_STAGE_GET_PLATFORM_DISPLAY 4
#define DMABUF_POOL_STAGE_INITIALIZE 5
#define DMABUF_POOL_STAGE_CHOOSE_CONFIG 6
#define DMABUF_POOL_STAGE_CREATE_CONTEXT 7
#define DMABUF_POOL_STAGE_MAKE_CURRENT 8
#define DMABUF_POOL_STAGE_EXTENSIONS 9

void *dmabuf_pool_create(int *failure_stage) {
    *failure_stage = DMABUF_POOL_STAGE_SUCCESS;
    DmabufPool *pool = calloc(1, sizeof(DmabufPool));
    if (!pool) {
        return NULL;
    }
    pool->drm_file_descriptor = -1;
    pool->display = EGL_NO_DISPLAY;
    pool->context = EGL_NO_CONTEXT;

    pool->drm_file_descriptor = open("/dev/dri/renderD128", O_RDWR);
    if (pool->drm_file_descriptor < 0) {
        *failure_stage = DMABUF_POOL_STAGE_OPEN_RENDER_NODE;
        dmabuf_pool_destroy(pool);
        return NULL;
    }

    pool->gbm_device = gbm_create_device(pool->drm_file_descriptor);
    if (!pool->gbm_device) {
        *failure_stage = DMABUF_POOL_STAGE_CREATE_GBM_DEVICE;
        dmabuf_pool_destroy(pool);
        return NULL;
    }

    PFNEGLGETPLATFORMDISPLAYEXTPROC get_platform_display =
        (PFNEGLGETPLATFORMDISPLAYEXTPROC)eglGetProcAddress("eglGetPlatformDisplayEXT");
    if (!get_platform_display) {
        *failure_stage = DMABUF_POOL_STAGE_GET_PLATFORM_DISPLAY_PROC;
        dmabuf_pool_destroy(pool);
        return NULL;
    }
    pool->display = get_platform_display(EGL_PLATFORM_GBM_KHR, pool->gbm_device, NULL);
    if (pool->display == EGL_NO_DISPLAY) {
        *failure_stage = DMABUF_POOL_STAGE_GET_PLATFORM_DISPLAY;
        dmabuf_pool_destroy(pool);
        return NULL;
    }

    EGLint major = 0;
    EGLint minor = 0;
    if (!eglInitialize(pool->display, &major, &minor)) {
        *failure_stage = DMABUF_POOL_STAGE_INITIALIZE;
        pool->display = EGL_NO_DISPLAY;
        dmabuf_pool_destroy(pool);
        return NULL;
    }
    eglBindAPI(EGL_OPENGL_ES_API);

    EGLint config_attributes[] = {EGL_SURFACE_TYPE,    EGL_WINDOW_BIT,
                                  EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT,
                                  EGL_RED_SIZE,        8,
                                  EGL_GREEN_SIZE,      8,
                                  EGL_BLUE_SIZE,       8,
                                  EGL_ALPHA_SIZE,      8,
                                  EGL_NONE};
    EGLConfig config;
    EGLint config_count = 0;
    if (!eglChooseConfig(pool->display, config_attributes, &config, 1, &config_count)
        || config_count == 0) {
        *failure_stage = DMABUF_POOL_STAGE_CHOOSE_CONFIG;
        dmabuf_pool_destroy(pool);
        return NULL;
    }

    EGLint context_attributes[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
    pool->context = eglCreateContext(pool->display, config, EGL_NO_CONTEXT, context_attributes);
    if (pool->context == EGL_NO_CONTEXT) {
        *failure_stage = DMABUF_POOL_STAGE_CREATE_CONTEXT;
        dmabuf_pool_destroy(pool);
        return NULL;
    }
    // OSR にウィンドウは無いので surfaceless で current にする。
    if (!eglMakeCurrent(pool->display, EGL_NO_SURFACE, EGL_NO_SURFACE, pool->context)) {
        *failure_stage = DMABUF_POOL_STAGE_MAKE_CURRENT;
        dmabuf_pool_destroy(pool);
        return NULL;
    }

    pool->create_image = (PFNEGLCREATEIMAGEKHRPROC)eglGetProcAddress("eglCreateImageKHR");
    pool->destroy_image = (PFNEGLDESTROYIMAGEKHRPROC)eglGetProcAddress("eglDestroyImageKHR");
    pool->image_target_texture =
        (PFNGLEGLIMAGETARGETTEXTURE2DOESPROC)eglGetProcAddress("glEGLImageTargetTexture2DOES");
    if (!pool->create_image || !pool->destroy_image || !pool->image_target_texture) {
        *failure_stage = DMABUF_POOL_STAGE_EXTENSIONS;
        dmabuf_pool_destroy(pool);
        return NULL;
    }

    return pool;
}

void dmabuf_pool_destroy(void *handle) {
    DmabufPool *pool = (DmabufPool *)handle;
    if (!pool) {
        return;
    }
    if (pool->output_framebuffer) {
        glDeleteFramebuffers(1, &pool->output_framebuffer);
    }
    if (pool->output_texture) {
        glDeleteTextures(1, &pool->output_texture);
    }
    if (pool->output_image && pool->destroy_image) {
        pool->destroy_image(pool->display, pool->output_image);
    }
    if (pool->output_buffer_object) {
        gbm_bo_destroy(pool->output_buffer_object);
    }
    if (pool->context != EGL_NO_CONTEXT) {
        eglMakeCurrent(pool->display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
        eglDestroyContext(pool->display, pool->context);
    }
    if (pool->display != EGL_NO_DISPLAY) {
        eglTerminate(pool->display);
    }
    if (pool->gbm_device) {
        gbm_device_destroy(pool->gbm_device);
    }
    if (pool->drm_file_descriptor >= 0) {
        close(pool->drm_file_descriptor);
    }
    free(pool);
}
