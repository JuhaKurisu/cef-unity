using CefUnity;
using UnityEngine;

public class SampleScript : MonoBehaviour
{
    private void Start()
    {
        NativeMethods.cef_unity_init();
    }

    private void OnDestroy()
    {
        NativeMethods.cef_unity_shutdown();
    }
}