using CefUnity;
using UnityEngine;

public class SampleScript : MonoBehaviour
{
    private void Start()
    {
        try
        {
            var result = NativeMethods.cef_unity_init();
            Debug.Log($"[CefUnity] cef_unity_init returned {result}");
        }
        catch (System.Exception e)
        {
            Debug.LogError($"[CefUnity] Init failed: {e}");
        }
    }

    private void Update()
    {
        NativeMethods.cef_unity_tick();
    }

    private void OnDestroy()
    {
        NativeMethods.cef_unity_shutdown();
    }
}
