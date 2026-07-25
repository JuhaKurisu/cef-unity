namespace CefUnity.Viewer
{
    /// <summary>ImeBridge の出力先 (テストでは fake、実行時は Browser アダプタ)。</summary>
    public interface IImeSink
    {
        void SetComposition(string text, uint cursorPosition);
        void CommitText(string text);
        void SendCharacter(char character);
        void FinishComposition();
        void CancelComposition();
    }

    /// <summary>
    ///     SDL TEXTEDITING/TEXTINPUT ⇔ CEF IME API の状態機械 (spec §ImeBridge)。
    ///     変換中 (Composing) の TEXTINPUT は確定、非変換の TEXTINPUT は素の文字入力。
    /// </summary>
    public sealed class ImeBridge
    {
        private readonly IImeSink _sink;

        public ImeBridge(IImeSink sink)
        {
            _sink = sink;
        }

        public bool Composing { get; private set; }

        public void OnTextEditing(string text, int cursorStart)
        {
            if (text.Length > 0)
            {
                Composing = true;
                _sink.SetComposition(text, (uint)cursorStart);
            }
            else if (Composing)
            {
                Composing = false;
                _sink.CancelComposition();
            }
        }

        public void OnTextInput(string text)
        {
            if (Composing)
            {
                Composing = false;
                _sink.CommitText(text);
            }
            else
            {
                foreach (var character in text) _sink.SendCharacter(character);
            }
        }

        public void OnFocusLost()
        {
            if (!Composing) return;
            Composing = false;
            _sink.FinishComposition();
        }
    }
}
