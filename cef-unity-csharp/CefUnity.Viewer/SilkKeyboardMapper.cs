using CefUnity.Interop;
using Silk.NET.Input;

namespace CefUnity.Viewer
{
    /// <summary>
    ///     Silk.NET Key → CefKeyCode 変換。Unity 用 CefKeyboardMapper は UnityEngine.KeyCode
    ///     依存のため流用不可 (spec 参照)。文字入力自体は SDL TEXTINPUT (ImeBridge) が担い、
    ///     ここは物理キー (RawKeyDown/KeyUp) とショートカット用。
    /// </summary>
    public static class SilkKeyboardMapper
    {
        private static readonly Dictionary<Key, CefKeyCode> Table = BuildTable();

        public static bool TryMap(Key key, out CefKeyCode code) => Table.TryGetValue(key, out code);

        /// <summary>修飾キー状態を CefEventFlags に変換する (送信時に毎回呼ぶ)。</summary>
        public static uint BuildModifiers(IKeyboard keyboard)
        {
            var flags = CefEventFlags.None;
            if (keyboard.IsKeyPressed(Key.ShiftLeft) || keyboard.IsKeyPressed(Key.ShiftRight)) flags |= CefEventFlags.ShiftDown;
            if (keyboard.IsKeyPressed(Key.ControlLeft) || keyboard.IsKeyPressed(Key.ControlRight)) flags |= CefEventFlags.ControlDown;
            if (keyboard.IsKeyPressed(Key.AltLeft) || keyboard.IsKeyPressed(Key.AltRight)) flags |= CefEventFlags.AltDown;
            if (keyboard.IsKeyPressed(Key.SuperLeft) || keyboard.IsKeyPressed(Key.SuperRight)) flags |= CefEventFlags.CommandDown;
            return (uint)flags;
        }

        private static Dictionary<Key, CefKeyCode> BuildTable()
        {
            var table = new Dictionary<Key, CefKeyCode>
            {
                // 特殊キーは Core の CefKeyCodes 定義をそのまま使う
                [Key.Backspace] = CefKeyCodes.Backspace,
                [Key.Tab] = CefKeyCodes.Tab,
                [Key.Enter] = CefKeyCodes.Return,
                [Key.Escape] = CefKeyCodes.Escape,
                [Key.Delete] = CefKeyCodes.Delete,
                [Key.Insert] = CefKeyCodes.Insert,
                [Key.Up] = CefKeyCodes.UpArrow,
                [Key.Down] = CefKeyCodes.DownArrow,
                [Key.Left] = CefKeyCodes.LeftArrow,
                [Key.Right] = CefKeyCodes.RightArrow,
                [Key.Home] = CefKeyCodes.Home,
                [Key.End] = CefKeyCodes.End,
                [Key.PageUp] = CefKeyCodes.PageUp,
                [Key.PageDown] = CefKeyCodes.PageDown,
                [Key.F1] = CefKeyCodes.F1, [Key.F2] = CefKeyCodes.F2, [Key.F3] = CefKeyCodes.F3,
                [Key.F4] = CefKeyCodes.F4, [Key.F5] = CefKeyCodes.F5, [Key.F6] = CefKeyCodes.F6,
                [Key.F7] = CefKeyCodes.F7, [Key.F8] = CefKeyCodes.F8, [Key.F9] = CefKeyCodes.F9,
                [Key.F10] = CefKeyCodes.F10, [Key.F11] = CefKeyCodes.F11, [Key.F12] = CefKeyCodes.F12,
                [Key.Keypad0] = CefKeyCodes.Keypad0, [Key.Keypad1] = CefKeyCodes.Keypad1,
                [Key.Keypad2] = CefKeyCodes.Keypad2, [Key.Keypad3] = CefKeyCodes.Keypad3,
                [Key.Keypad4] = CefKeyCodes.Keypad4, [Key.Keypad5] = CefKeyCodes.Keypad5,
                [Key.Keypad6] = CefKeyCodes.Keypad6, [Key.Keypad7] = CefKeyCodes.Keypad7,
                [Key.Keypad8] = CefKeyCodes.Keypad8, [Key.Keypad9] = CefKeyCodes.Keypad9,
                [Key.KeypadDecimal] = CefKeyCodes.KeypadPeriod,
                [Key.KeypadDivide] = CefKeyCodes.KeypadDivide,
                [Key.KeypadMultiply] = CefKeyCodes.KeypadMultiply,
                [Key.KeypadSubtract] = CefKeyCodes.KeypadMinus,
                [Key.KeypadAdd] = CefKeyCodes.KeypadPlus,
                [Key.KeypadEnter] = CefKeyCodes.KeypadEnter,
                [Key.ShiftLeft] = CefKeyCodes.LeftShift, [Key.ShiftRight] = CefKeyCodes.RightShift,
                [Key.ControlLeft] = CefKeyCodes.LeftControl, [Key.ControlRight] = CefKeyCodes.RightControl,
                [Key.AltLeft] = CefKeyCodes.LeftAlt, [Key.AltRight] = CefKeyCodes.RightAlt,
                [Key.SuperLeft] = CefKeyCodes.LeftCommand, [Key.SuperRight] = CefKeyCodes.RightCommand,
                [Key.CapsLock] = CefKeyCodes.CapsLock,
                [Key.Space] = new CefKeyCode(0x20, 49, ' '),
                // 記号 (Windows VK_OEM_* / mac kVK_ANSI_*)
                [Key.Minus] = new CefKeyCode(0xBD, 27, '-'),
                [Key.Equal] = new CefKeyCode(0xBB, 24, '='),
                [Key.LeftBracket] = new CefKeyCode(0xDB, 33, '['),
                [Key.RightBracket] = new CefKeyCode(0xDD, 30, ']'),
                [Key.BackSlash] = new CefKeyCode(0xDC, 42, '\\'),
                [Key.Semicolon] = new CefKeyCode(0xBA, 41, ';'),
                [Key.Apostrophe] = new CefKeyCode(0xDE, 39, '\''),
                [Key.Comma] = new CefKeyCode(0xBC, 43, ','),
                [Key.Period] = new CefKeyCode(0xBE, 47, '.'),
                [Key.Slash] = new CefKeyCode(0xBF, 44, '/'),
                [Key.GraveAccent] = new CefKeyCode(0xC0, 50, '`'),
            };
            // 英字: windowsKeyCode は 'A'..'Z'、mac native は kVK_ANSI_* 標準表
            int[] letterNativeCodes =
            {
                0, 11, 8, 2, 14, 3, 5, 4, 34, 38, 40, 37, 46,      // A B C D E F G H I J K L M
                45, 31, 35, 12, 15, 1, 17, 32, 9, 13, 7, 16, 6,    // N O P Q R S T U V W X Y Z
            };
            for (var index = 0; index < 26; index++)
                table[Key.A + index] = new CefKeyCode('A' + index, letterNativeCodes[index], (char)('a' + index));
            // 数字: windowsKeyCode は '0'..'9'、mac native は kVK_ANSI_0..9
            int[] digitNativeCodes = { 29, 18, 19, 20, 21, 23, 22, 26, 28, 25 }; // 0 1 2 3 4 5 6 7 8 9
            for (var index = 0; index < 10; index++)
                table[Key.Number0 + index] = new CefKeyCode('0' + index, digitNativeCodes[index], (char)('0' + index));
            return table;
        }
    }
}
