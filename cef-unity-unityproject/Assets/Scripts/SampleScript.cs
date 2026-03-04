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
    private Texture2D _texture;
    private float _diagTimer;

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
        _browser.SendMouseMove();
    }

    private void UpdateTexture()
    {
        if (_browser == null) return;
        if (!_browser.TryGetBuffer(out var buffer, out var w, out var h))
            return;

        if (w <= 0 || h <= 0) return;

        if (_texture == null || _texture.width != w || _texture.height != h)
        {
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

        _texture.Apply();
    }

    private void OnDestroy()
    {
        _browser?.Dispose();
        _browser = null;
        CefRuntime.Shutdown();
        Debug.Log("[CefUnity] Shutdown");
    }
}