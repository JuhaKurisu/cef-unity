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
#include <linux/dma-buf.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

// CEF のテクスチャを出力バッファへ写すだけの最小シェーダ。
// 頂点は正規化デバイス座標の全画面クアッドで、UV は Y を反転させてある
// (CEF/Chromium のテクスチャ原点は左上、GL のテクスチャ原点は左下)。
static const char *VERTEX_SHADER_SOURCE =
    "attribute vec2 position;\n"
    "varying vec2 texture_coordinate;\n"
    "void main() {\n"
    "  texture_coordinate = vec2((position.x + 1.0) * 0.5, (1.0 - position.y) * 0.5);\n"
    "  gl_Position = vec4(position, 0.0, 1.0);\n"
    "}\n";

// CEF が渡す dmabuf は GL_TEXTURE_2D では黒くしかサンプルできない。
// EGLImage 由来のテクスチャは GL_TEXTURE_EXTERNAL_OES とし、samplerExternalOES で読む
// (GL_OES_EGL_image_external)。出力バッファ側は描画対象なので通常の 2D のまま。
static const char *FRAGMENT_SHADER_SOURCE =
    "#extension GL_OES_EGL_image_external : require\n"
    "precision mediump float;\n"
    "varying vec2 texture_coordinate;\n"
    "uniform samplerExternalOES source_texture;\n"
    "void main() {\n"
    "  gl_FragColor = texture2D(source_texture, texture_coordinate);\n"
    "}\n";

typedef struct {
    int drm_file_descriptor;
    struct gbm_device *gbm_device;
    EGLDisplay display;
    EGLContext context;

    GLuint program;
    GLint position_attribute;
    GLint source_texture_uniform;

    struct gbm_bo *output_buffer_object;
    EGLImageKHR output_image;
    GLuint output_texture;
    GLuint output_framebuffer;
    int output_width;
    int output_height;
    unsigned int generation;

    // 診断: 入力テクスチャの中心ピクセルと FBO の状態。
    unsigned char source_inspect_result[4];
    int source_inspect_status;
    // 診断: クリア直後の即時読み戻し。
    unsigned char clear_check_result[4];
    int clear_check_error;
    int clear_check_status;

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

// on_accelerated_paint は CEF のスレッドで呼ばれ、そのスレッドでは Chromium 自身の
// GL コンテキストが current になっていることがある。自分のコンテキストへ切り替えずに
// GL を呼ぶと、描画が別のコンテキストへ流れて黙って消える (実測: 赤でクリアしても
// 出力バッファが変化しなかった)。呼び出しの前後で必ず退避・復帰する。
typedef struct {
    EGLDisplay display;
    EGLContext context;
    EGLSurface draw_surface;
    EGLSurface read_surface;
} SavedEglState;

static void save_and_make_current(DmabufPool *pool, SavedEglState *saved) {
    saved->display = eglGetCurrentDisplay();
    saved->context = eglGetCurrentContext();
    saved->draw_surface = eglGetCurrentSurface(EGL_DRAW);
    saved->read_surface = eglGetCurrentSurface(EGL_READ);
    eglMakeCurrent(pool->display, EGL_NO_SURFACE, EGL_NO_SURFACE, pool->context);
}

static void restore_current(SavedEglState *saved) {
    if (saved->display != EGL_NO_DISPLAY && saved->context != EGL_NO_CONTEXT) {
        eglMakeCurrent(saved->display, saved->draw_surface, saved->read_surface, saved->context);
    }
}

static GLuint compile_shader(GLenum type, const char *source) {
    GLuint shader = glCreateShader(type);
    glShaderSource(shader, 1, &source, NULL);
    glCompileShader(shader);
    GLint compiled = 0;
    glGetShaderiv(shader, GL_COMPILE_STATUS, &compiled);
    if (!compiled) {
        glDeleteShader(shader);
        return 0;
    }
    return shader;
}

// シェーダは 1 度だけ作って使い回す。
static int ensure_program(DmabufPool *pool) {
    if (pool->program) {
        return 1;
    }
    GLuint vertex_shader = compile_shader(GL_VERTEX_SHADER, VERTEX_SHADER_SOURCE);
    GLuint fragment_shader = compile_shader(GL_FRAGMENT_SHADER, FRAGMENT_SHADER_SOURCE);
    if (!vertex_shader || !fragment_shader) {
        return 0;
    }
    GLuint program = glCreateProgram();
    glAttachShader(program, vertex_shader);
    glAttachShader(program, fragment_shader);
    glLinkProgram(program);
    glDeleteShader(vertex_shader);
    glDeleteShader(fragment_shader);

    GLint linked = 0;
    glGetProgramiv(program, GL_LINK_STATUS, &linked);
    if (!linked) {
        glDeleteProgram(program);
        return 0;
    }
    pool->program = program;
    pool->position_attribute = glGetAttribLocation(program, "position");
    pool->source_texture_uniform = glGetUniformLocation(program, "source_texture");
    return 1;
}

// dmabuf を EGLImage 経由で GL テクスチャにする。失敗したら 0。
static GLuint import_dmabuf_texture(DmabufPool *pool, EGLImageKHR *out_image, GLenum target,
                                    int file_descriptor, unsigned int stride,
                                    unsigned long long offset, unsigned long long modifier,
                                    unsigned int fourcc, int width, int height) {
    // modifier を明示すると NVIDIA ではリニアバッファのサンプリングが空になることがある。
    // 0 (DRM_FORMAT_MOD_LINEAR) のときは属性ごと省いてドライバに委ねる。
    EGLint image_attributes_with_modifier[] = {
        EGL_WIDTH,                          width,
        EGL_HEIGHT,                         height,
        EGL_LINUX_DRM_FOURCC_EXT,           (EGLint)fourcc,
        EGL_DMA_BUF_PLANE0_FD_EXT,          file_descriptor,
        EGL_DMA_BUF_PLANE0_OFFSET_EXT,      (EGLint)offset,
        EGL_DMA_BUF_PLANE0_PITCH_EXT,       (EGLint)stride,
        EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT, (EGLint)(modifier & 0xffffffff),
        EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT, (EGLint)(modifier >> 32),
        EGL_NONE};
    EGLint image_attributes_without_modifier[] = {
        EGL_WIDTH,                     width,
        EGL_HEIGHT,                    height,
        EGL_LINUX_DRM_FOURCC_EXT,      (EGLint)fourcc,
        EGL_DMA_BUF_PLANE0_FD_EXT,     file_descriptor,
        EGL_DMA_BUF_PLANE0_OFFSET_EXT, (EGLint)offset,
        EGL_DMA_BUF_PLANE0_PITCH_EXT,  (EGLint)stride,
        EGL_NONE};
    EGLint *image_attributes =
        (modifier == 0) ? image_attributes_without_modifier : image_attributes_with_modifier;
    EGLImageKHR image = pool->create_image(pool->display, EGL_NO_CONTEXT,
                                           EGL_LINUX_DMA_BUF_EXT, NULL, image_attributes);
    if (image == EGL_NO_IMAGE_KHR) {
        return 0;
    }
    GLuint texture = 0;
    glGenTextures(1, &texture);
    glBindTexture(target, texture);
    glTexParameteri(target, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glTexParameteri(target, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glTexParameteri(target, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glTexParameteri(target, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    pool->image_target_texture(target, image);
    if (glGetError() != GL_NO_ERROR) {
        glDeleteTextures(1, &texture);
        pool->destroy_image(pool->display, image);
        return 0;
    }
    *out_image = image;
    return texture;
}

// 出力バッファはサイズが変わったときだけ作り直す (d3d11_pool.rs と同じ方針)。
static int ensure_output(DmabufPool *pool, int width, int height) {
    if (pool->output_buffer_object && pool->output_width == width
        && pool->output_height == height) {
        return 1;
    }
    if (pool->output_framebuffer) {
        glDeleteFramebuffers(1, &pool->output_framebuffer);
        pool->output_framebuffer = 0;
    }
    if (pool->output_texture) {
        glDeleteTextures(1, &pool->output_texture);
        pool->output_texture = 0;
    }
    if (pool->output_image) {
        pool->destroy_image(pool->display, pool->output_image);
        pool->output_image = NULL;
    }
    if (pool->output_buffer_object) {
        gbm_bo_destroy(pool->output_buffer_object);
        pool->output_buffer_object = NULL;
    }

    // リニア modifier は NVIDIA の GBM で確保に失敗するため要求しない。
    pool->output_buffer_object = gbm_bo_create(pool->gbm_device, width, height,
                                               GBM_FORMAT_ARGB8888, GBM_BO_USE_RENDERING);
    if (!pool->output_buffer_object) {
        return 0;
    }

    int output_file_descriptor = gbm_bo_get_fd(pool->output_buffer_object);
    if (output_file_descriptor < 0) {
        return 0;
    }
    EGLImageKHR image = NULL;
    pool->output_texture = import_dmabuf_texture(
        pool, &image, GL_TEXTURE_2D, output_file_descriptor,
        gbm_bo_get_stride(pool->output_buffer_object), gbm_bo_get_offset(pool->output_buffer_object, 0),
        gbm_bo_get_modifier(pool->output_buffer_object), GBM_FORMAT_ARGB8888, width, height);
    // EGLImage が参照を保持するので、ここで閉じてよい。
    close(output_file_descriptor);
    if (!pool->output_texture) {
        return 0;
    }
    pool->output_image = image;

    glGenFramebuffers(1, &pool->output_framebuffer);
    glBindFramebuffer(GL_FRAMEBUFFER, pool->output_framebuffer);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D,
                           pool->output_texture, 0);
    if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) {
        return 0;
    }

    pool->output_width = width;
    pool->output_height = height;
    pool->generation += 1;
    return 1;
}

int dmabuf_pool_blit(void *handle, int source_file_descriptor, unsigned int source_stride,
                     unsigned long long source_offset, unsigned long long source_modifier,
                     unsigned int source_fourcc, int width, int height) {
    DmabufPool *pool = (DmabufPool *)handle;
    if (!pool) {
        return 0;
    }
    SavedEglState saved;
    save_and_make_current(pool, &saved);
    if (!ensure_program(pool) || !ensure_output(pool, width, height)) {
        restore_current(&saved);
        return 0;
    }

    EGLImageKHR source_image = NULL;
    GLuint source_texture =
        import_dmabuf_texture(pool, &source_image, GL_TEXTURE_EXTERNAL_OES, source_file_descriptor,
                              source_stride,
                              source_offset, source_modifier, source_fourcc, width, height);
    if (!source_texture) {
        restore_current(&saved);
        return 0;
    }

    glBindFramebuffer(GL_FRAMEBUFFER, pool->output_framebuffer);
    glViewport(0, 0, width, height);
    // 診断 (一時): 出力 FBO が本当に共有バッファを指しているかを確かめるため、
    // 描画前に既知の色でクリアする。読み戻しが赤なら FBO は正しく、
    // 問題は入力テクスチャのサンプリング側にある。
    if (getenv("CEF_UNITY_DEBUG_CLEAR")) {
        glClearColor(1.0f, 0.0f, 0.0f, 1.0f);
        glClear(GL_COLOR_BUFFER_BIT);
        glFinish();
        // 同じコンテキスト・同じ FBO のまま即座に読み戻す。
        // ここで赤が読めなければ書き込み自体が届いていない。
        static int clear_checked = 0;
        if (!clear_checked) {
            clear_checked = 1;
            glReadPixels(width / 2, height / 2, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE,
                         pool->clear_check_result);
            pool->clear_check_error = (int)glGetError();
            pool->clear_check_status = (int)glCheckFramebufferStatus(GL_FRAMEBUFFER);
        }
    }
    glUseProgram(pool->program);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_EXTERNAL_OES, source_texture);
    glUniform1i(pool->source_texture_uniform, 0);

    static const GLfloat QUAD[] = {-1.0f, -1.0f, 1.0f, -1.0f, -1.0f, 1.0f, 1.0f, 1.0f};
    glEnableVertexAttribArray((GLuint)pool->position_attribute);
    glVertexAttribPointer((GLuint)pool->position_attribute, 2, GL_FLOAT, GL_FALSE, 0, QUAD);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    glDisableVertexAttribArray((GLuint)pool->position_attribute);

    // macOS の waitUntilCompleted と同じ位置づけ。完了を待ってから frame_id を進める。
    glFinish();

    glDeleteTextures(1, &source_texture);
    pool->destroy_image(pool->display, source_image);
    restore_current(&saved);
    return 1;
}

int dmabuf_pool_output(void *handle, int *out_file_descriptor, unsigned int *out_stride,
                       unsigned long long *out_modifier, unsigned int *out_fourcc,
                       unsigned int *out_generation, int *out_width, int *out_height) {
    DmabufPool *pool = (DmabufPool *)handle;
    if (!pool || !pool->output_buffer_object) {
        return 0;
    }
    // 呼び出し側が閉じる。
    *out_file_descriptor = gbm_bo_get_fd(pool->output_buffer_object);
    if (*out_file_descriptor < 0) {
        return 0;
    }
    *out_stride = gbm_bo_get_stride(pool->output_buffer_object);
    *out_modifier = gbm_bo_get_modifier(pool->output_buffer_object);
    *out_fourcc = GBM_FORMAT_ARGB8888;
    *out_generation = pool->generation;
    *out_width = pool->output_width;
    *out_height = pool->output_height;
    return 1;
}

// テスト専用: ソースバッファを単色で塗る。blit がピクセルを運んでいるかの検証に使う。
int dmabuf_pool_fill_test_source(void *handle, int file_descriptor, unsigned int stride,
                                 unsigned long long modifier, unsigned int fourcc, int width,
                                 int height, unsigned char red, unsigned char green,
                                 unsigned char blue, unsigned char alpha) {
    unsigned long long offset = 0;
    DmabufPool *pool = (DmabufPool *)handle;
    if (!pool) {
        return 0;
    }
    EGLImageKHR image = NULL;
    GLuint texture = import_dmabuf_texture(pool, &image, GL_TEXTURE_2D, file_descriptor, stride,
                                           offset, modifier, fourcc, width, height);
    if (!texture) {
        return 0;
    }
    GLuint framebuffer = 0;
    glGenFramebuffers(1, &framebuffer);
    glBindFramebuffer(GL_FRAMEBUFFER, framebuffer);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, texture, 0);
    int complete = glCheckFramebufferStatus(GL_FRAMEBUFFER) == GL_FRAMEBUFFER_COMPLETE;
    if (complete) {
        glViewport(0, 0, width, height);
        glClearColor(red / 255.0f, green / 255.0f, blue / 255.0f, alpha / 255.0f);
        glClear(GL_COLOR_BUFFER_BIT);
        glFinish();
    }
    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glDeleteFramebuffers(1, &framebuffer);
    glDeleteTextures(1, &texture);
    pool->destroy_image(pool->display, image);
    return complete;
}

// 診断: 入力テクスチャの検査結果を返す。1 = 読めた、それ以外は FBO のステータス。
// 診断: CEF の dmabuf を CPU から直接読む。GL の取り込みを疑う前に
// 「そもそもデータが入っているか」を確定させるための経路。
// 戻り値 1 = 読めた。out_rgba にはリニア配置前提で中心ピクセルが入る。
int dmabuf_read_center_via_cpu(int file_descriptor, unsigned int stride,
                               unsigned long long offset, int width, int height,
                               unsigned char *out_rgba) {
    size_t length = (size_t)offset + (size_t)stride * (size_t)height;
    void *mapped = mmap(NULL, length, PROT_READ, MAP_SHARED, file_descriptor, 0);
    if (mapped == MAP_FAILED) {
        return 0;
    }
    // dmabuf の CPU 読みは DMA_BUF_IOCTL_SYNC で囲まないとキャッシュ不整合で
    // 古い内容 (ゼロ) を見ることがある。
    struct dma_buf_sync sync_arguments = {DMA_BUF_SYNC_START | DMA_BUF_SYNC_READ};
    ioctl(file_descriptor, DMA_BUF_IOCTL_SYNC, &sync_arguments);
    unsigned char *base = (unsigned char *)mapped + offset;
    unsigned char *pixel = base + (size_t)(height / 2) * stride + (size_t)(width / 2) * 4;
    out_rgba[0] = pixel[0];
    out_rgba[1] = pixel[1];
    out_rgba[2] = pixel[2];
    out_rgba[3] = pixel[3];
    // バッファ全体を走査して非ゼロが 1 バイトでもあるかを数える。
    // 中心だけ見て「空」と判断しないため。
    size_t non_zero = 0;
    for (int row = 0; row < height; row++) {
        unsigned char *line = base + (size_t)row * stride;
        for (int column = 0; column < width * 4; column++) {
            if (line[column] != 0) non_zero++;
        }
    }
    out_rgba[4] = (unsigned char)(non_zero > 0);
    *((size_t *)(out_rgba + 8)) = non_zero;
    struct dma_buf_sync sync_end = {DMA_BUF_SYNC_END | DMA_BUF_SYNC_READ};
    ioctl(file_descriptor, DMA_BUF_IOCTL_SYNC, &sync_end);
    munmap(mapped, length);
    return 1;
}

// 診断: クリア直後の即時読み戻し結果。
int dmabuf_pool_clear_check(void *handle, unsigned char *out_rgba, int *out_error,
                            int *out_status) {
    DmabufPool *pool = (DmabufPool *)handle;
    if (!pool) return 0;
    out_rgba[0] = pool->clear_check_result[0];
    out_rgba[1] = pool->clear_check_result[1];
    out_rgba[2] = pool->clear_check_result[2];
    out_rgba[3] = pool->clear_check_result[3];
    *out_error = pool->clear_check_error;
    *out_status = pool->clear_check_status;
    return 1;
}

int dmabuf_pool_source_inspect(void *handle, unsigned char *out_rgba) {
    DmabufPool *pool = (DmabufPool *)handle;
    if (!pool) return 0;
    out_rgba[0] = pool->source_inspect_result[0];
    out_rgba[1] = pool->source_inspect_result[1];
    out_rgba[2] = pool->source_inspect_result[2];
    out_rgba[3] = pool->source_inspect_result[3];
    return pool->source_inspect_status;
}

// 出力バッファの 1 ピクセルを読み戻す。テストと、blit が効いているかの診断に使う。
int dmabuf_pool_read_output_pixel(void *handle, int x, int y, unsigned char *out_rgba) {
    DmabufPool *pool = (DmabufPool *)handle;
    if (!pool || !pool->output_framebuffer) {
        return 0;
    }
    SavedEglState saved;
    save_and_make_current(pool, &saved);
    glBindFramebuffer(GL_FRAMEBUFFER, pool->output_framebuffer);
    glReadPixels(x, y, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, out_rgba);
    int ok = glGetError() == GL_NO_ERROR;
    restore_current(&saved);
    return ok;
}

// テスト専用: CEF から渡されたものに見立てたソースバッファを確保する。
// gbm_bo は破棄するが、エクスポートした fd が参照を保持するのでメモリは生きている。
int dmabuf_pool_create_test_source(void *handle, int width, int height,
                                   int *out_file_descriptor, unsigned int *out_stride,
                                   unsigned long long *out_modifier, unsigned int *out_fourcc) {
    DmabufPool *pool = (DmabufPool *)handle;
    if (!pool) {
        return 0;
    }
    struct gbm_bo *buffer_object = gbm_bo_create(pool->gbm_device, width, height,
                                                 GBM_FORMAT_ARGB8888, GBM_BO_USE_RENDERING);
    if (!buffer_object) {
        return 0;
    }
    *out_file_descriptor = gbm_bo_get_fd(buffer_object);
    *out_stride = gbm_bo_get_stride(buffer_object);
    *out_modifier = gbm_bo_get_modifier(buffer_object);
    *out_fourcc = GBM_FORMAT_ARGB8888;
    gbm_bo_destroy(buffer_object);
    return *out_file_descriptor >= 0;
}

void dmabuf_pool_destroy(void *handle) {
    DmabufPool *pool = (DmabufPool *)handle;
    if (!pool) {
        return;
    }
    if (pool->program) {
        glDeleteProgram(pool->program);
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
