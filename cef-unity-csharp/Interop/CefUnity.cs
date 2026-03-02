using CefUnity;

namespace Interop;

public class Browser
{
    private unsafe CefUnityBrowser* _browser;

    public unsafe Browser(CefUnityBrowser* browser)
    {
        _browser = browser;
    }
}

public static class CefUnity
{
    public static void Init()
    {
        NativeMethods.cef_unity_init();
    }

    public static void Tick()
    {
        NativeMethods.cef_unity_tick();
    }

    public static void CreateBrowser(int width, int height, string url)
    {
        
    }
}