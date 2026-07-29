using CefUnity.Interop;
using UnityEngine;
#if UNITY_STANDALONE_OSX || UNITY_EDITOR_OSX
using System;
using System.Runtime.InteropServices;
#elif UNITY_STANDALONE_WIN || UNITY_EDITOR_WIN
using System.Runtime.InteropServices;
#endif

namespace CefUnity.Runtime
{
    /// <summary>
    ///     Unity KeyCode → CEF キーコードの対応表と、OS のキーリピートパラメータ取得
    ///     (static データ/純関数)。入力ハンドラ (MonoBehaviour) から独立に参照できる。
    /// </summary>
    public static class CefKeyboardMapper
    {
        /// <summary>Unity KeyCode → CefKeyCode の対応テーブル (非印字キー)。</summary>
        public static readonly (KeyCode unity, CefKeyCode cef)[] SpecialKeyTable =
        {
            (KeyCode.Backspace, CefKeyCodes.Backspace),
            (KeyCode.Tab, CefKeyCodes.Tab),
            (KeyCode.Return, CefKeyCodes.Return),
            (KeyCode.Escape, CefKeyCodes.Escape),
            (KeyCode.Delete, CefKeyCodes.Delete),
            (KeyCode.Insert, CefKeyCodes.Insert),

            (KeyCode.UpArrow, CefKeyCodes.UpArrow),
            (KeyCode.DownArrow, CefKeyCodes.DownArrow),
            (KeyCode.LeftArrow, CefKeyCodes.LeftArrow),
            (KeyCode.RightArrow, CefKeyCodes.RightArrow),
            (KeyCode.Home, CefKeyCodes.Home),
            (KeyCode.End, CefKeyCodes.End),
            (KeyCode.PageUp, CefKeyCodes.PageUp),
            (KeyCode.PageDown, CefKeyCodes.PageDown),

            (KeyCode.F1, CefKeyCodes.F1), (KeyCode.F2, CefKeyCodes.F2),
            (KeyCode.F3, CefKeyCodes.F3), (KeyCode.F4, CefKeyCodes.F4),
            (KeyCode.F5, CefKeyCodes.F5), (KeyCode.F6, CefKeyCodes.F6),
            (KeyCode.F7, CefKeyCodes.F7), (KeyCode.F8, CefKeyCodes.F8),
            (KeyCode.F9, CefKeyCodes.F9), (KeyCode.F10, CefKeyCodes.F10),
            (KeyCode.F11, CefKeyCodes.F11), (KeyCode.F12, CefKeyCodes.F12),

            (KeyCode.Keypad0, CefKeyCodes.Keypad0), (KeyCode.Keypad1, CefKeyCodes.Keypad1),
            (KeyCode.Keypad2, CefKeyCodes.Keypad2), (KeyCode.Keypad3, CefKeyCodes.Keypad3),
            (KeyCode.Keypad4, CefKeyCodes.Keypad4), (KeyCode.Keypad5, CefKeyCodes.Keypad5),
            (KeyCode.Keypad6, CefKeyCodes.Keypad6), (KeyCode.Keypad7, CefKeyCodes.Keypad7),
            (KeyCode.Keypad8, CefKeyCodes.Keypad8), (KeyCode.Keypad9, CefKeyCodes.Keypad9),
            (KeyCode.KeypadPeriod, CefKeyCodes.KeypadPeriod),
            (KeyCode.KeypadDivide, CefKeyCodes.KeypadDivide),
            (KeyCode.KeypadMultiply, CefKeyCodes.KeypadMultiply),
            (KeyCode.KeypadMinus, CefKeyCodes.KeypadMinus),
            (KeyCode.KeypadPlus, CefKeyCodes.KeypadPlus),
            (KeyCode.KeypadEnter, CefKeyCodes.KeypadEnter),

            (KeyCode.LeftShift, CefKeyCodes.LeftShift),
            (KeyCode.RightShift, CefKeyCodes.RightShift),
            (KeyCode.LeftControl, CefKeyCodes.LeftControl),
            (KeyCode.RightControl, CefKeyCodes.RightControl),
            (KeyCode.LeftAlt, CefKeyCodes.LeftAlt),
            (KeyCode.RightAlt, CefKeyCodes.RightAlt),
            (KeyCode.LeftCommand, CefKeyCodes.LeftCommand),
            (KeyCode.RightCommand, CefKeyCodes.RightCommand),
            (KeyCode.CapsLock, CefKeyCodes.CapsLock)
        };

        /// <summary>長押しリピート開始までの遅延 (秒)。macOS はシステム設定値、他は 0.5s。</summary>
        public static readonly float KeyRepeatDelay = GetOSKeyRepeatDelay();

        /// <summary>リピート間隔 (秒)。macOS はシステム設定値、他は 0.035s。</summary>
        public static readonly float KeyRepeatRate = GetOSKeyRepeatRate();

#if UNITY_STANDALONE_OSX || UNITY_EDITOR_OSX
        [DllImport("/usr/lib/libobjc.dylib", EntryPoint = "objc_getClass")]
        private static extern IntPtr ObjcGetClass([MarshalAs(UnmanagedType.LPStr)] string name);

        [DllImport("/usr/lib/libobjc.dylib", EntryPoint = "sel_registerName")]
        private static extern IntPtr ObjcSelectorRegisterName([MarshalAs(UnmanagedType.LPStr)] string name);

        [DllImport("/usr/lib/libobjc.dylib", EntryPoint = "objc_msgSend")]
        private static extern double ObjcMessageSendDouble(IntPtr receiver, IntPtr selector);

        private static float GetOSKeyRepeatDelay()
        {
            try
            {
                var nsEvent = ObjcGetClass("NSEvent");
                var selector = ObjcSelectorRegisterName("keyRepeatDelay");
                var value = ObjcMessageSendDouble(nsEvent, selector);
                return value > 0 ? (float)value : 0.5f;
            }
            catch
            {
                return 0.5f;
            }
        }

        private static float GetOSKeyRepeatRate()
        {
            try
            {
                var nsEvent = ObjcGetClass("NSEvent");
                var selector = ObjcSelectorRegisterName("keyRepeatInterval");
                var value = ObjcMessageSendDouble(nsEvent, selector);
                return value > 0 ? (float)value : 0.035f;
            }
            catch
            {
                return 0.035f;
            }
        }
#elif UNITY_STANDALONE_WIN || UNITY_EDITOR_WIN
        [DllImport("user32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool SystemParametersInfoW(
            uint action, uint parameter, out uint value, uint update);

        private const uint SpiGetKeyboardDelay = 0x0016;
        private const uint SpiGetKeyboardSpeed = 0x000A;

        /// <summary>
        ///     SPI_GETKEYBOARDDELAY は 0〜3 の段階値で、(値 + 1) × 250ms を表す
        ///     (0 = 250ms 〜 3 = 1000ms)。取得できなければ macOS 側と同じ既定値。
        /// </summary>
        private static float GetOSKeyRepeatDelay()
        {
            try
            {
                if (SystemParametersInfoW(SpiGetKeyboardDelay, 0, out var value, 0) && value <= 3)
                    return (value + 1) * 0.25f;
            }
            catch (System.Exception)
            {
                // P/Invoke 不能環境では既定値へ落とす
            }
            return 0.5f;
        }

        /// <summary>
        ///     SPI_GETKEYBOARDSPEED は 0〜31 の段階値で、約 2.5〜30 回/秒に線形対応する。
        ///     間隔 (秒) はその逆数。取得できなければ macOS 側と同じ既定値。
        /// </summary>
        private static float GetOSKeyRepeatRate()
        {
            try
            {
                if (SystemParametersInfoW(SpiGetKeyboardSpeed, 0, out var value, 0) && value <= 31)
                {
                    var repeatsPerSecond = 2.5f + value * ((30f - 2.5f) / 31f);
                    return 1f / repeatsPerSecond;
                }
            }
            catch (System.Exception)
            {
            }
            return 0.035f;
        }
#else
        private static float GetOSKeyRepeatDelay()
        {
            return 0.5f;
        }

        private static float GetOSKeyRepeatRate()
        {
            return 0.035f;
        }
#endif
    }
}
