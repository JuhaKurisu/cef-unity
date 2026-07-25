namespace CefUnity.Viewer
{
    /// <summary>CEF SendMouseClick の clickCount 算出 (500ms・4px 以内の連打で加算)。</summary>
    public sealed class ClickCounter
    {
        private const double MaxIntervalSeconds = 0.5;
        private const int MaxDistancePixels = 4;

        private double _lastTimestamp = double.NegativeInfinity;
        private int _lastX;
        private int _lastY;
        private int _clickCount;

        public int OnMouseDown(double timestampSeconds, int x, int y)
        {
            var withinTime = timestampSeconds - _lastTimestamp <= MaxIntervalSeconds;
            var withinDistance = Math.Abs(x - _lastX) <= MaxDistancePixels && Math.Abs(y - _lastY) <= MaxDistancePixels;
            _clickCount = withinTime && withinDistance ? _clickCount + 1 : 1;
            _lastTimestamp = timestampSeconds;
            _lastX = x;
            _lastY = y;
            return _clickCount;
        }
    }
}
