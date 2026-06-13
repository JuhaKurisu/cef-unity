using CefUnity.Runtime;
using UnityEngine;
using UnityEngine.UI;

public class Sample : MonoBehaviour
{
    [SerializeField] private CefUnityBrowserSample _browser;
    [SerializeField] private bool _urlNavigateTest;
    [SerializeField] private Image _image;

    private void Update()
    {
        if (_urlNavigateTest)
        {
            _browser.LoadUrl("https://www.youtube.com/watch?v=dQw4w9WgXcQ");
            _urlNavigateTest = false;
        }

        _image.rectTransform.anchoredPosition = Input.mousePosition;
    }
}