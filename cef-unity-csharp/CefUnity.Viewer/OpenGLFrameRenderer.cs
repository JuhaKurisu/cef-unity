using Silk.NET.OpenGLES;
using Silk.NET.Windowing;

namespace CefUnity.Viewer
{
    /// <summary>
    ///     受信した GL テクスチャを既定フレームバッファへ描いて表示する
    ///     (macOS の MetalFrameRenderer、Windows の D3D11FrameRenderer に対応する Linux 実装)。
    ///
    ///     テクスチャの中身はサーバが dmabuf へ blit したもので、クライアント側は
    ///     `cef_unity_get_dmabuf_texture` が返す GL テクスチャ名を受け取る
    ///     (crates/client/src/dmabuf.rs)。ゼロコピーなのでここでピクセルは触らない。
    ///
    ///     Y 方向: サーバ側の blit で既に反転済みなので、ここでは反転しない
    ///     (crates/server/src/dmabuf_pool.c の頂点シェーダ)。
    ///
    ///     GLES を使うのは意図的。SDL は GLES 要求時に EGL コンテキストを作り、
    ///     dmabuf の取り込みが EGL を要求するため (ViewerWindow の graphicsApi 参照)。
    /// </summary>
    internal sealed class OpenGLFrameRenderer : IFrameRenderer
    {
        private const string VertexShaderSource = @"#version 300 es
layout (location = 0) in vec2 position;
out vec2 textureCoordinate;
void main() {
    textureCoordinate = (position + vec2(1.0)) * 0.5;
    gl_Position = vec4(position, 0.0, 1.0);
}";

        private const string FragmentShaderSource = @"#version 300 es
precision mediump float;
in vec2 textureCoordinate;
out vec4 fragmentColor;
uniform sampler2D sourceTexture;
void main() {
    fragmentColor = texture(sourceTexture, textureCoordinate);
}";

        private static readonly float[] FullScreenQuad =
        {
            -1.0f, -1.0f,
             1.0f, -1.0f,
            -1.0f,  1.0f,
             1.0f,  1.0f,
        };

        /// <summary>
        ///     表示内容の自動検証用。環境変数で指定すると、指定フレーム描画後に
        ///     既定フレームバッファを読み戻して PNG に書き出す。
        ///     GNOME はスクリーンショットの D-Bus 呼び出しを拒否するため、
        ///     実際に画面へ出た内容を確かめる手段としてこちらを用意している。
        /// </summary>
        private readonly string? _capturePath = Environment.GetEnvironmentVariable("CEFUNITY_CAPTURE");
        private int _presentedFrames;
        private bool _captured;

        private GL? _gl;
        private IView? _view;
        private uint _program;
        private uint _vertexArray;
        private uint _vertexBuffer;
        private int _sourceTextureLocation;

        public void Initialize(IView view)
        {
            _view = view;
            _gl = GL.GetApi(view);

            var vertexShader = CompileShader(ShaderType.VertexShader, VertexShaderSource);
            var fragmentShader = CompileShader(ShaderType.FragmentShader, FragmentShaderSource);
            _program = _gl.CreateProgram();
            _gl.AttachShader(_program, vertexShader);
            _gl.AttachShader(_program, fragmentShader);
            _gl.LinkProgram(_program);
            _gl.GetProgram(_program, ProgramPropertyARB.LinkStatus, out var linked);
            if (linked == 0)
            {
                throw new InvalidOperationException(
                    $"表示用シェーダのリンクに失敗した: {_gl.GetProgramInfoLog(_program)}");
            }
            _gl.DeleteShader(vertexShader);
            _gl.DeleteShader(fragmentShader);
            _sourceTextureLocation = _gl.GetUniformLocation(_program, "sourceTexture");

            _vertexArray = _gl.GenVertexArray();
            _gl.BindVertexArray(_vertexArray);
            _vertexBuffer = _gl.GenBuffer();
            _gl.BindBuffer(BufferTargetARB.ArrayBuffer, _vertexBuffer);
            unsafe
            {
                fixed (float* quad = FullScreenQuad)
                {
                    _gl.BufferData(BufferTargetARB.ArrayBuffer,
                                   (nuint)(FullScreenQuad.Length * sizeof(float)),
                                   quad, BufferUsageARB.StaticDraw);
                }
                _gl.VertexAttribPointer(0, 2, VertexAttribPointerType.Float, false,
                                        (uint)(2 * sizeof(float)), (void*)0);
            }
            _gl.EnableVertexAttribArray(0);
            _gl.BindVertexArray(0);
        }

        public void Present(IntPtr texturePointer, int width, int height)
        {
            if (_gl is null || _view is null)
            {
                return;
            }

            var framebufferSize = _view.FramebufferSize;
            _gl.Viewport(0, 0, (uint)framebufferSize.X, (uint)framebufferSize.Y);
            _gl.ClearColor(0.0f, 0.0f, 0.0f, 1.0f);
            _gl.Clear(ClearBufferMask.ColorBufferBit);

            // texturePointer が 0 のときは blit せず drawable を回すだけ
            // (IFrameRenderer の既存の契約。起動直後とスパイク用)。
            var texture = (uint)texturePointer.ToInt64();
            if (texture == 0 || width <= 0 || height <= 0)
            {
                return;
            }

            _gl.UseProgram(_program);
            _gl.ActiveTexture(TextureUnit.Texture0);
            _gl.BindTexture(TextureTarget.Texture2D, texture);
            _gl.Uniform1(_sourceTextureLocation, 0);
            _gl.BindVertexArray(_vertexArray);
            _gl.DrawArrays(PrimitiveType.TriangleStrip, 0, 4);
            _gl.BindVertexArray(0);

            _presentedFrames++;
            // ページのロードと最初の描画が落ち着いてから撮る。
            if (_capturePath != null && !_captured && _presentedFrames >= 120)
            {
                _captured = true;
                InspectSourceTexture(texture, width, height);
                CaptureDefaultFramebuffer(framebufferSize.X, framebufferSize.Y);
            }
        }

        /// <summary>
        ///     取り込んだテクスチャ自体の中身を読み戻す。画面が黒いときに
        ///     「テクスチャが黒い」のか「描画が効いていない」のかを切り分ける。
        /// </summary>
        private void InspectSourceTexture(uint texture, int width, int height)
        {
            var framebuffer = _gl!.GenFramebuffer();
            _gl.BindFramebuffer(FramebufferTarget.Framebuffer, framebuffer);
            _gl.FramebufferTexture2D(FramebufferTarget.Framebuffer, FramebufferAttachment.ColorAttachment0,
                                     TextureTarget.Texture2D, texture, 0);
            var status = _gl.CheckFramebufferStatus(FramebufferTarget.Framebuffer);
            if (status == GLEnum.FramebufferComplete)
            {
                var pixel = new byte[4];
                unsafe
                {
                    fixed (byte* buffer = pixel)
                    {
                        _gl.ReadPixels(width / 2, height / 2, 1, 1,
                                       PixelFormat.Rgba, PixelType.UnsignedByte, buffer);
                    }
                }
                Console.WriteLine($"SOURCE_TEXTURE center RGBA=({pixel[0]},{pixel[1]},{pixel[2]},{pixel[3]}) size={width}x{height}");
            }
            else
            {
                Console.WriteLine($"SOURCE_TEXTURE FBO incomplete: 0x{(int)status:X}");
            }
            _gl.BindFramebuffer(FramebufferTarget.Framebuffer, 0);
            _gl.DeleteFramebuffer(framebuffer);
            Console.WriteLine($"GL error after draw: 0x{(int)_gl.GetError():X}");
        }

        /// <summary>既定フレームバッファを読み戻して PNG に書き出す。</summary>
        private void CaptureDefaultFramebuffer(int width, int height)
        {
            var pixels = new byte[width * height * 4];
            unsafe
            {
                fixed (byte* buffer = pixels)
                {
                    _gl!.ReadPixels(0, 0, (uint)width, (uint)height,
                                    PixelFormat.Rgba, PixelType.UnsignedByte, buffer);
                }
            }
            // glReadPixels は下から上、RGBA。PNG ライターは上から下、BGRA を要求する。
            var bgra = new byte[pixels.Length];
            for (var row = 0; row < height; row++)
            {
                var sourceRow = (height - 1 - row) * width * 4;
                var destinationRow = row * width * 4;
                for (var column = 0; column < width; column++)
                {
                    var source = sourceRow + column * 4;
                    var destination = destinationRow + column * 4;
                    bgra[destination + 0] = pixels[source + 2];
                    bgra[destination + 1] = pixels[source + 1];
                    bgra[destination + 2] = pixels[source + 0];
                    bgra[destination + 3] = pixels[source + 3];
                }
            }
            CefUnity.Harness.PortableNetworkGraphicsWriter.WriteBgra(_capturePath!, bgra, width, height);
            Console.WriteLine($"CAPTURE_OK {_capturePath} {width}x{height}");
        }

        private uint CompileShader(ShaderType type, string source)
        {
            var shader = _gl!.CreateShader(type);
            _gl.ShaderSource(shader, source);
            _gl.CompileShader(shader);
            _gl.GetShader(shader, ShaderParameterName.CompileStatus, out var compiled);
            if (compiled == 0)
            {
                throw new InvalidOperationException(
                    $"表示用シェーダのコンパイルに失敗した: {_gl.GetShaderInfoLog(shader)}");
            }
            return shader;
        }

        public void Dispose()
        {
            if (_gl is null)
            {
                return;
            }
            if (_vertexBuffer != 0) _gl.DeleteBuffer(_vertexBuffer);
            if (_vertexArray != 0) _gl.DeleteVertexArray(_vertexArray);
            if (_program != 0) _gl.DeleteProgram(_program);
            _gl = null;
        }
    }
}
