using System.Collections.Generic;
using CefUnity.Viewer;
using NUnit.Framework;

namespace CefUnity.Tests
{
    [TestFixture]
    public class ImeBridgeTests
    {
        private sealed class RecordingSink : IImeSink
        {
            public readonly List<string> Calls = new();
            public void SetComposition(string text, uint cursorPosition) => Calls.Add($"set:{text}:{cursorPosition}");
            public void CommitText(string text) => Calls.Add($"commit:{text}");
            public void SendCharacter(char character) => Calls.Add($"char:{character}");
            public void FinishComposition() => Calls.Add("finish");
            public void CancelComposition() => Calls.Add("cancel");
        }

        [Test]
        public void AsciiTyping_NoComposition_SendsCharEvents()
        {
            var sink = new RecordingSink();
            var bridge = new ImeBridge(sink);
            bridge.OnTextInput("ab");
            Assert.That(sink.Calls, Is.EqualTo(new[] { "char:a", "char:b" }));
            Assert.That(bridge.Composing, Is.False);
        }

        [Test]
        public void JapaneseComposition_EditThenCommit()
        {
            var sink = new RecordingSink();
            var bridge = new ImeBridge(sink);
            bridge.OnTextEditing("か", 1);
            bridge.OnTextEditing("かん", 2);
            bridge.OnTextInput("漢");
            Assert.That(sink.Calls, Is.EqualTo(new[] { "set:か:1", "set:かん:2", "commit:漢" }));
            Assert.That(bridge.Composing, Is.False);
        }

        [Test]
        public void EmptyEditingDuringComposition_Cancels()
        {
            var sink = new RecordingSink();
            var bridge = new ImeBridge(sink);
            bridge.OnTextEditing("か", 1);
            bridge.OnTextEditing("", 0); // Esc で変換破棄
            Assert.That(sink.Calls, Is.EqualTo(new[] { "set:か:1", "cancel" }));
            Assert.That(bridge.Composing, Is.False);
        }

        [Test]
        public void FocusLostDuringComposition_Finishes()
        {
            var sink = new RecordingSink();
            var bridge = new ImeBridge(sink);
            bridge.OnTextEditing("か", 1);
            bridge.OnFocusLost();
            Assert.That(sink.Calls, Is.EqualTo(new[] { "set:か:1", "finish" }));
            Assert.That(bridge.Composing, Is.False);
        }

        [Test]
        public void FocusLostWithoutComposition_DoesNothing()
        {
            var sink = new RecordingSink();
            var bridge = new ImeBridge(sink);
            bridge.OnFocusLost();
            Assert.That(sink.Calls, Is.Empty);
        }
    }
}
