using System;
using CefUnity;
using CefUnity.Interop;
using UnityEngine;

public class SampleScript : MonoBehaviour
{
    [SerializeField] private int _width;
    [SerializeField] private int _height;
    [SerializeField] private string _url;
    private Browser _browser;
    
    private void Start()
    {
        try
        {
            var result = NativeMethods.cef_unity_init();
            Debug.Log($"[CefUnity] cef_unity_init returned {result}");
            _browser = new Browser(_width, _height, _url);
        }
        catch (System.Exception e)
        {
            Debug.LogError($"[CefUnity] Init failed: {e}");
        }
    }

    private void Update()
    {
    }

    private void OnDestroy()
    {
        NativeMethods.cef_unity_shutdown();
    }
}
