using Silk.NET.OpenGL;
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
    /// </summary>
    internal sealed class OpenGLFrameRenderer : IFrameRenderer
    {
        private const string VertexShaderSource = @"#version 330 core
layout (location = 0) in vec2 position;
out vec2 textureCoordinate;
void main() {
    textureCoordinate = (position + vec2(1.0)) * 0.5;
    gl_Position = vec4(position, 0.0, 1.0);
}";

        private const string FragmentShaderSource = @"#version 330 core
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
