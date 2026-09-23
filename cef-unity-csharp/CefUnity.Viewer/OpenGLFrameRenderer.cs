using Silk.NET.OpenGLES;
using Silk.NET.Windowing;

namespace CefUnity.Viewer
{
    /// <summary>
    ///     GL テクスチャを既定フレームバッファへ全画面クアッドで描いて表示する
    ///     (macOS の MetalFrameRenderer、Windows の D3D11FrameRenderer に対応する Linux 実装)。
    ///
    ///     Linux の CEF は software paint なので、テクスチャの中身は CPU の BGRA バッファを
    ///     毎フレーム (新しいフレームが来たときだけ) アップロードしたもの。
    ///     GLES は BGRA のアップロードを拡張なしでは受けないため RGBA として上げ、
    ///     並び替えはフラグメントシェーダで行う。
    ///
    ///     Y 方向: CEF のバッファは上から下、GL のテクスチャ座標は下から上なので
    ///     頂点シェーダで反転する。
    ///
    ///     GLES を使うのは、SDL が GLES 要求時に EGL コンテキストを作るため
    ///     (将来の dmabuf 取り込み glEGLImageTargetTexture2DOES が EGL を要求する)。
    /// </summary>
    internal sealed class OpenGLFrameRenderer : IFrameRenderer, IBgraFrameUploader
    {
        private const string VertexShaderSource = @"#version 300 es
layout (location = 0) in vec2 position;
out vec2 textureCoordinate;
void main() {
    textureCoordinate = vec2((position.x + 1.0) * 0.5, (1.0 - position.y) * 0.5);
    gl_Position = vec4(position, 0.0, 1.0);
}";

        private const string FragmentShaderSource = @"#version 300 es
precision mediump float;
in vec2 textureCoordinate;
out vec4 fragmentColor;
uniform sampler2D sourceTexture;
void main() {
    fragmentColor = texture(sourceTexture, textureCoordinate).bgra;
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
        private uint _uploadTexture;
        private int _uploadWidth;
        private int _uploadHeight;

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

            _uploadTexture = _gl.GenTexture();
            _gl.BindTexture(TextureTarget.Texture2D, _uploadTexture);
            // ウィンドウとブラウザは同じサイズなので等倍表示。リサイズ過渡期の拡縮だけ線形で均す
            _gl.TexParameter(TextureTarget.Texture2D, TextureParameterName.TextureMinFilter, (int)TextureMinFilter.Linear);
            _gl.TexParameter(TextureTarget.Texture2D, TextureParameterName.TextureMagFilter, (int)TextureMagFilter.Linear);
            _gl.TexParameter(TextureTarget.Texture2D, TextureParameterName.TextureWrapS, (int)TextureWrapMode.ClampToEdge);
            _gl.TexParameter(TextureTarget.Texture2D, TextureParameterName.TextureWrapT, (int)TextureWrapMode.ClampToEdge);
            _gl.BindTexture(TextureTarget.Texture2D, 0);
        }

        public IntPtr UploadBgra(ReadOnlySpan<byte> bgra, int width, int height)
        {
            if (_gl is null || width <= 0 || height <= 0 || bgra.Length < width * height * 4)
            {
                return IntPtr.Zero;
            }

            _gl.BindTexture(TextureTarget.Texture2D, _uploadTexture);
            // 行末パディングは無い (width * 4 バイト詰め) が、既定の 4 バイト境界でも一致するので
            // UNPACK_ALIGNMENT は触らない
            unsafe
            {
                fixed (byte* pixels = bgra)
                {
                    if (width != _uploadWidth || height != _uploadHeight)
                    {
                        _gl.TexImage2D(TextureTarget.Texture2D, 0, InternalFormat.Rgba8,
                                       (uint)width, (uint)height, 0,
                                       PixelFormat.Rgba, PixelType.UnsignedByte, pixels);
                        _uploadWidth = width;
                        _uploadHeight = height;
                    }
                    else
                    {
                        _gl.TexSubImage2D(TextureTarget.Texture2D, 0, 0, 0,
                                          (uint)width, (uint)height,
                                          PixelFormat.Rgba, PixelType.UnsignedByte, pixels);
                    }
                }
            }
            _gl.BindTexture(TextureTarget.Texture2D, 0);
            return (IntPtr)_uploadTexture;
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

            // texturePointer が 0 のときは描かずにクリアだけ行う
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
                CaptureDefaultFramebuffer(framebufferSize.X, framebufferSize.Y);
            }
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
            if (_uploadTexture != 0) _gl.DeleteTexture(_uploadTexture);
            if (_vertexBuffer != 0) _gl.DeleteBuffer(_vertexBuffer);
            if (_vertexArray != 0) _gl.DeleteVertexArray(_vertexArray);
            if (_program != 0) _gl.DeleteProgram(_program);
            _gl = null;
        }
    }
}
