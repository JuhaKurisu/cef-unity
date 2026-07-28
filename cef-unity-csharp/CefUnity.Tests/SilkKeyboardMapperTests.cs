using CefUnity.Interop;
using CefUnity.Viewer;
using NUnit.Framework;
using Silk.NET.Input;

namespace CefUnity.Tests
{
    [TestFixture]
    public class SilkKeyboardMapperTests
    {
        [TestCase(Key.A, 0x41, 0)]
        [TestCase(Key.Z, 0x5A, 6)]
        [TestCase(Key.Number0, 0x30, 29)]
        [TestCase(Key.Number1, 0x31, 18)]
        [TestCase(Key.Space, 0x20, 49)]
        public void TryMap_PrintableKey_ReturnsWindowsAndNativeCode(Key key, int expectedWindowsKeyCode, int expectedNativeKeyCode)
        {
            Assert.That(SilkKeyboardMapper.TryMap(key, out var code), Is.True);
            Assert.That(code.WindowsKeyCode, Is.EqualTo(expectedWindowsKeyCode));
            Assert.That(code.NativeKeyCode, Is.EqualTo(expectedNativeKeyCode));
        }

        [Test]
        public void TryMap_SpecialKeys_UseCefKeyCodesTable()
        {
            Assert.That(SilkKeyboardMapper.TryMap(Key.Enter, out var enter), Is.True);
            Assert.That(enter.WindowsKeyCode, Is.EqualTo(CefKeyCodes.Return.WindowsKeyCode));
            Assert.That(SilkKeyboardMapper.TryMap(Key.Backspace, out var backspace), Is.True);
            Assert.That(backspace.WindowsKeyCode, Is.EqualTo(CefKeyCodes.Backspace.WindowsKeyCode));
            Assert.That(SilkKeyboardMapper.TryMap(Key.Up, out var up), Is.True);
            Assert.That(up.WindowsKeyCode, Is.EqualTo(CefKeyCodes.UpArrow.WindowsKeyCode));
            Assert.That(SilkKeyboardMapper.TryMap(Key.PageDown, out var pageDown), Is.True);
            Assert.That(pageDown.WindowsKeyCode, Is.EqualTo(CefKeyCodes.PageDown.WindowsKeyCode));
        }

        [Test]
        public void TryMap_UnknownKey_ReturnsFalse()
        {
            Assert.That(SilkKeyboardMapper.TryMap(Key.Unknown, out _), Is.False);
        }
    }
}
