using CefUnity.Viewer;
using NUnit.Framework;

namespace CefUnity.Tests
{
    /// <summary>表示バックエンド選択のプラットフォーム判定 (OS 依存を引数に切り出してテストする)。</summary>
    [TestFixture]
    public class FrameRendererFactoryTests
    {
        [Test]
        public void SelectKind_MacOS_ReturnsMetal()
            => Assert.That(FrameRendererFactory.SelectKind(isMacOS: true, isWindows: false, isLinux: false),
                           Is.EqualTo(FrameRendererKind.Metal));

        [Test]
        public void SelectKind_Windows_ReturnsDirect3D11()
            => Assert.That(FrameRendererFactory.SelectKind(isMacOS: false, isWindows: true, isLinux: false),
                           Is.EqualTo(FrameRendererKind.Direct3D11));

        [Test]
        public void SelectKind_Linux_ReturnsOpenGL()
            => Assert.That(FrameRendererFactory.SelectKind(isMacOS: false, isWindows: false, isLinux: true),
                           Is.EqualTo(FrameRendererKind.OpenGL));

        [Test]
        public void SelectKind_OtherPlatform_ReturnsUnsupported()
            => Assert.That(FrameRendererFactory.SelectKind(isMacOS: false, isWindows: false, isLinux: false),
                           Is.EqualTo(FrameRendererKind.Unsupported));
    }
}
