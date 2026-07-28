namespace CefUnity.Viewer
{
    public enum ScrollMode
    {
        Raw,
        Smoother,
        Resampler,
    }

    /// <summary>CLI 引数。解析失敗は null (呼び出し側が Usage を表示)。</summary>
    public sealed class ViewerOptions
    {
        public const string Usage =
            "usage: CefUnity.Viewer [--url <url>] [--size <width>x<height>]\n" +
            "                       [--scroll-mode raw|smoother|resampler] [--record]\n" +
            "                       [--replay <events-csv>] [--statistics <output-csv>]\n" +
            "                       [--analyze <statistics-csv>]";

        public string Url { get; private set; } = "https://example.com";
        public int Width { get; private set; } = 1280;
        public int Height { get; private set; } = 720;
        public ScrollMode Mode { get; private set; } = ScrollMode.Resampler;
        public bool Record { get; private set; }
        public string? ReplayPath { get; private set; }
        public string? StatisticsPath { get; private set; }
        public string? AnalyzePath { get; private set; }

        public static ViewerOptions? Parse(string[] arguments)
        {
            var options = new ViewerOptions();
            for (var index = 0; index < arguments.Length; index++)
            {
                string? Next() => index + 1 < arguments.Length ? arguments[++index] : null;
                switch (arguments[index])
                {
                    case "--url":
                        if (Next() is not { } url) return null;
                        options.Url = url;
                        break;
                    case "--size":
                        var size = Next()?.Split('x');
                        if (size is not { Length: 2 }
                            || !int.TryParse(size[0], out var width)
                            || !int.TryParse(size[1], out var height)) return null;
                        options.Width = width;
                        options.Height = height;
                        break;
                    case "--scroll-mode":
                        switch (Next())
                        {
                            case "raw": options.Mode = ScrollMode.Raw; break;
                            case "smoother": options.Mode = ScrollMode.Smoother; break;
                            case "resampler": options.Mode = ScrollMode.Resampler; break;
                            default: return null;
                        }
                        break;
                    case "--record":
                        options.Record = true;
                        break;
                    case "--replay":
                        if (Next() is not { } replayPath) return null;
                        options.ReplayPath = replayPath;
                        break;
                    case "--statistics":
                        if (Next() is not { } statisticsPath) return null;
                        options.StatisticsPath = statisticsPath;
                        break;
                    case "--analyze":
                        if (Next() is not { } analyzePath) return null;
                        options.AnalyzePath = analyzePath;
                        break;
                    default:
                        return null;
                }
            }
            return options;
        }
    }
}
