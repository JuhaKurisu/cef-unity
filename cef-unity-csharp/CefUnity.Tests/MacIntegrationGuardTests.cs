using System.Runtime.InteropServices;
using CefUnity.Viewer;
using NUnit.Framework;

namespace CefUnity.Tests
{
    /// <summary>
    ///     mac 専用の Obj-C interop が非 macOS で例外を投げないこと (OS ガードの回帰テスト)。
    ///     ガードが無いと Windows では Program 起動直後に DllNotFoundException で落ちる。
    /// </summary>
    [TestFixture]
    public class MacIntegrationGuardTests
    {
        [Test]
        public void Enable_OnNonMacOS_DoesNotThrow()
        {
            if (RuntimeInformation.IsOSPlatform(OSPlatform.OSX))
                Assert.Ignore("macOS では実際に NSUserDefaults を触るため対象外");
            Assert.DoesNotThrow(() => MacMomentumScrollSupport.Enable());
        }

        [Test]
        public void ActivateCurrentApplication_OnNonMacOS_DoesNotThrow()
        {
            if (RuntimeInformation.IsOSPlatform(OSPlatform.OSX))
                Assert.Ignore("macOS では実際に NSApplication を触るため対象外");
            Assert.DoesNotThrow(() => MacApplicationActivator.ActivateCurrentApplication());
        }
    }
}
