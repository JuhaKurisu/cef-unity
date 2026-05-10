using System;
using CefUnity.Runtime;
using UnityEngine;

public class Sample : MonoBehaviour
{
    [SerializeField] private CefUnityBrowserSample _browser;

    private void Update()
    {
        if (Input.GetKeyDown(KeyCode.L))
        {
            _browser.LoadUrl("https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        }
    }
}
