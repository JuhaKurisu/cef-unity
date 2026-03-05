using System;
using CefUnity;
using CefUnity.Interop;
using UnityEngine;
using UnityEngine.UI;

public class SampleScript : MonoBehaviour
{
    [SerializeField] private int _width = 1280;
    [SerializeField] private int _height = 720;
    [SerializeField] private string _url = "https://www.google.com";
    [SerializeField] private RawImage _rawImage;

    private Browser _browser;
    private float _diagTimer;
    private Texture2D _texture;
    private int _lastMouseX = -1;
    private int _lastMouseY = -1;

    private void Start()
    {
        try
        {
            CefRuntime.Init();
            _browser = new Browser(_width, _height, _url);
            Debug.Log("[CefUnity] Initialized");
        }
        catch (Exception e)
        {
            Debug.LogError($"[CefUnity] Init failed: {e}");
        }
    }

    private void Update()
    {
        CefRuntime.Pump();

        _diagTimer += Time.deltaTime;
        if (_diagTimer >= 2f)
        {
            _diagTimer = 0f;
            var paintCount = NativeMethods.cef_unity_get_paint_count();
            var pumpCount = NativeMethods.cef_unity_get_pump_count();
            Debug.Log($"[CefUnity] diag: paint={paintCount} pump={pumpCount}");
        }

        UpdateTexture();
        HandleMouseInput();
        HandleKeyboardInput();
    }

    private void OnDestroy()
    {
        _browser?.Dispose();
        _browser = null;

        if (_texture != null)
        {
            Destroy(_texture);
            _texture = null;
        }

        CefRuntime.Shutdown();
        Debug.Log("[CefUnity] Shutdown");
    }

    private void HandleMouseInput()
    {
        if (_browser == null || _rawImage == null) return;

        if (!TryGetBrowserCoord(out var bx, out var by))
            return;

        // Mouse move (座標が変わった時だけ送信)
        if (bx != _lastMouseX || by != _lastMouseY)
        {
            _lastMouseX = bx;
            _lastMouseY = by;
            _browser.SendMouseMove(bx, by);
        }

        // Mouse buttons: Left=0, Right=1, Middle=2
        HandleButton(bx, by, 0, MouseButton.Left);
        HandleButton(bx, by, 1, MouseButton.Right);
        HandleButton(bx, by, 2, MouseButton.Middle);

        // Mouse wheel
        var scroll = Input.mouseScrollDelta;
        if (scroll.y != 0f || scroll.x != 0f) _browser.SendMouseWheel(bx, by, (int)(scroll.x * 120), (int)(scroll.y * 120));
    }

    private void HandleButton(int bx, int by, int unityButton, MouseButton cefButton)
    {
        if (Input.GetMouseButtonDown(unityButton))
            _browser.SendMouseClick(bx, by, cefButton, false);
        if (Input.GetMouseButtonUp(unityButton))
            _browser.SendMouseClick(bx, by, cefButton, true);
    }

    /// <summary>
    ///     スクリーン上のマウス座標を RawImage のローカル座標経由でブラウザ座標 (0..width, 0..height) に変換する。
    ///     RawImage 外なら false を返す。
    /// </summary>
    private bool TryGetBrowserCoord(out int bx, out int by)
    {
        bx = by = 0;
        var rt = _rawImage.rectTransform;

        // Canvas 内の Camera を取得（Overlay なら null）
        var canvas = _rawImage.canvas;
        var cam = canvas.renderMode == RenderMode.ScreenSpaceOverlay ? null : canvas.worldCamera;

        if (!RectTransformUtility.ScreenPointToLocalPointInRectangle(
                rt, Input.mousePosition, cam, out var local))
            return false;

        var rect = rt.rect;
        // rect 内の 0..1 正規化座標
        var nx = (local.x - rect.x) / rect.width;
        var ny = (local.y - rect.y) / rect.height;

        if (nx < 0f || nx > 1f || ny < 0f || ny > 1f)
            return false;

        // uvRect (0,1,1,-1) で Y 反転しているので補正
        ny = 1f - ny;

        bx = Mathf.Clamp((int)(nx * _width), 0, _width - 1);
        by = Mathf.Clamp((int)(ny * _height), 0, _height - 1);
        return true;
    }

    private void HandleKeyboardInput()
    {
        if (_browser == null) return;

        // 文字入力 (inputString は今フレームで入力された文字列)
        foreach (var c in Input.inputString)
        {
            var vk = CharToWindowsVk(c);
            _browser.SendKeyEvent(KeyEventType.RawKeyDown, vk, character: c, unmodifiedCharacter: c);
            _browser.SendKeyEvent(KeyEventType.Char, c, character: c, unmodifiedCharacter: c);
            _browser.SendKeyEvent(KeyEventType.KeyUp, vk, character: c, unmodifiedCharacter: c);
        }

        // 特殊キー (inputString に含まれないもの)
        HandleSpecialKey(KeyCode.LeftArrow, 0x25);
        HandleSpecialKey(KeyCode.RightArrow, 0x27);
        HandleSpecialKey(KeyCode.UpArrow, 0x26);
        HandleSpecialKey(KeyCode.DownArrow, 0x28);
        HandleSpecialKey(KeyCode.Home, 0x24);
        HandleSpecialKey(KeyCode.End, 0x23);
        HandleSpecialKey(KeyCode.PageUp, 0x21);
        HandleSpecialKey(KeyCode.PageDown, 0x22);
        HandleSpecialKey(KeyCode.F1, 0x70);
        HandleSpecialKey(KeyCode.F2, 0x71);
        HandleSpecialKey(KeyCode.F3, 0x72);
        HandleSpecialKey(KeyCode.F4, 0x73);
        HandleSpecialKey(KeyCode.F5, 0x74);
        HandleSpecialKey(KeyCode.F6, 0x75);
        HandleSpecialKey(KeyCode.F7, 0x76);
        HandleSpecialKey(KeyCode.F8, 0x77);
        HandleSpecialKey(KeyCode.F9, 0x78);
        HandleSpecialKey(KeyCode.F10, 0x79);
        HandleSpecialKey(KeyCode.F11, 0x7A);
        HandleSpecialKey(KeyCode.F12, 0x7B);
    }

    private void HandleSpecialKey(KeyCode key, int windowsVk)
    {
        if (Input.GetKeyDown(key))
            _browser.SendKeyEvent(KeyEventType.RawKeyDown, windowsVk);
        if (Input.GetKeyUp(key))
            _browser.SendKeyEvent(KeyEventType.KeyUp, windowsVk);
    }

    private static int CharToWindowsVk(char c)
    {
        return c switch
        {
            '\b' => 0x08, // Backspace
            '\t' => 0x09, // Tab
            '\r' or '\n' => 0x0D, // Enter
            '\x1b' => 0x1B, // Escape
            '\x7f' => 0x2E, // Delete
            >= 'a' and <= 'z' => c - 32, // VK_A..VK_Z
            >= 'A' and <= 'Z' => c, // VK_A..VK_Z
            >= '0' and <= '9' => c, // VK_0..VK_9
            ' ' => 0x20,
            _ => c,
        };
    }

    private void UpdateTexture()
    {
        if (_browser == null) return;

        // TryGetBuffer は新しいフレームがある場合のみ true を返す
        if (!_browser.TryGetBuffer(out var buffer, out var w, out var h))
            return;

        if (w <= 0 || h <= 0) return;

        if (_texture == null || _texture.width != w || _texture.height != h)
        {
            // 古いテクスチャを破棄して GPU メモリリークを防ぐ
            if (_texture != null)
                Destroy(_texture);

            _texture = new Texture2D(w, h, TextureFormat.BGRA32, false);
            if (_rawImage != null)
            {
                _rawImage.texture = _texture;
                _rawImage.uvRect = new Rect(0, 1, 1, -1);
            }
        }

        unsafe
        {
            fixed (byte* ptr = buffer)
            {
                _texture.LoadRawTextureData((IntPtr)ptr, buffer.Length);
            }
        }

        _texture.Apply(false);
    }
}