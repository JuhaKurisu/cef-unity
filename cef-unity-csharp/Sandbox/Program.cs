using CefUnity;

namespace Sandbox;

class Program
{
    static void Main(string[] args)
    {
        NativeMethods.cef_unity_init();
        NativeMethods.cef_unity_shutdown();
    }
}