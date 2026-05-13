using CefUnity.Runtime;
using UnityEngine;

public class Sample : MonoBehaviour
{
    [SerializeField] private CefUnityBrowserSample _browser;
    [SerializeField] private bool _urlNavigateTest;

    private void Update()
    {
        if (_urlNavigateTest)
        {
            _browser.LoadUrl("https://www.youtube.com/watch?v=dQw4w9WgXcQ");
            _urlNavigateTest = false;
        }
    }
}
