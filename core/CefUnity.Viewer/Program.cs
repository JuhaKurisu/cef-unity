using CefUnity.Viewer;

var viewerOptions = ViewerOptions.Parse(args);
if (viewerOptions == null)
{
    Console.Error.WriteLine(ViewerOptions.Usage);
    return 2;
}
Console.WriteLine($"CefUnity.Viewer options ok: {viewerOptions.Url} {viewerOptions.Width}x{viewerOptions.Height} mode={viewerOptions.Mode}");
return 0;
