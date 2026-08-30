// Linux: サーバから受け取った dmabuf をホストの GL コンテキストへ取り込む。
//
// ホスト (Viewer / Unity) が作った GL コンテキストが current な状態で呼ばれる前提。
// 自前でコンテキストを作らないのがサーバ側の dmabuf_pool.c との違い。

#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>
#include <stddef.h>
#include <stdint.h>

int dmabuf_import_texture(int file_descriptor, int width, int height, unsigned int stride,
                          unsigned long long modifier, unsigned int fourcc,
                          unsigned int *out_texture) {
    // 現在 current な EGLDisplay を使う。ホストが用意したものを借りる。
    EGLDisplay display = eglGetCurrentDisplay();
    if (display == EGL_NO_DISPLAY) {
        return 0;
    }

    PFNEGLCREATEIMAGEKHRPROC create_image =
        (PFNEGLCREATEIMAGEKHRPROC)eglGetProcAddress("eglCreateImageKHR");
    PFNGLEGLIMAGETARGETTEXTURE2DOESPROC image_target_texture =
        (PFNGLEGLIMAGETARGETTEXTURE2DOESPROC)eglGetProcAddress("glEGLImageTargetTexture2DOES");
    if (!create_image || !image_target_texture) {
        return 0;
    }

    EGLint image_attributes[] = {
        EGL_WIDTH,                          width,
        EGL_HEIGHT,                         height,
        EGL_LINUX_DRM_FOURCC_EXT,           (EGLint)fourcc,
        EGL_DMA_BUF_PLANE0_FD_EXT,          file_descriptor,
        EGL_DMA_BUF_PLANE0_OFFSET_EXT,      0,
        EGL_DMA_BUF_PLANE0_PITCH_EXT,       (EGLint)stride,
        EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT, (EGLint)(modifier & 0xffffffff),
        EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT, (EGLint)(modifier >> 32),
        EGL_NONE};
    EGLImageKHR image =
        create_image(display, EGL_NO_CONTEXT, EGL_LINUX_DMA_BUF_EXT, NULL, image_attributes);
    if (image == EGL_NO_IMAGE_KHR) {
        return 0;
    }

    GLuint texture = 0;
    glGenTextures(1, &texture);
    glBindTexture(GL_TEXTURE_2D, texture);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    image_target_texture(GL_TEXTURE_2D, image);

    PFNEGLDESTROYIMAGEKHRPROC destroy_image =
        (PFNEGLDESTROYIMAGEKHRPROC)eglGetProcAddress("eglDestroyImageKHR");
    if (glGetError() != GL_NO_ERROR) {
        glDeleteTextures(1, &texture);
        if (destroy_image) {
            destroy_image(display, image);
        }
        return 0;
    }
    // テクスチャが内容を参照し続けるため EGLImage 自体はここで解放してよい。
    if (destroy_image) {
        destroy_image(display, image);
    }

    *out_texture = texture;
    return 1;
}

void dmabuf_release_texture(unsigned int texture) {
    glDeleteTextures(1, &texture);
}
