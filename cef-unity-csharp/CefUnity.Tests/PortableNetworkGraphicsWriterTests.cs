using System;
using System.Buffers.Binary;
using System.IO;
using System.IO.Compression;
using System.Text;
using CefUnity.Harness;
using NUnit.Framework;

namespace CefUnity.Tests
{
    [TestFixture]
    public class PortableNetworkGraphicsWriterTests
    {
        private static readonly byte[] PngSignature = { 0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A };

        private static string WriteTemporaryPng(byte[] bgra, int width, int height)
        {
            var path = Path.Combine(Path.GetTempPath(), $"png-writer-test-{Guid.NewGuid():N}.png");
            PortableNetworkGraphicsWriter.WriteBgra(path, bgra, width, height);
            return path;
        }

        [Test]
        public void WriteBgra_WritesSignatureAndHeaderDimensions()
        {
            const int width = 3;
            const int height = 2;
            var path = WriteTemporaryPng(new byte[width * height * 4], width, height);
            try
            {
                var bytes = File.ReadAllBytes(path);
                Assert.That(bytes[..8], Is.EqualTo(PngSignature), "PNG シグネチャが一致しない");

                // 8..12 = IHDR の長さ, 12..16 = "IHDR", 16..20 = width, 20..24 = height
                Assert.That(BinaryPrimitives.ReadInt32BigEndian(bytes.AsSpan(8)), Is.EqualTo(13));
                Assert.That(Encoding.ASCII.GetString(bytes, 12, 4), Is.EqualTo("IHDR"));
                Assert.That(BinaryPrimitives.ReadInt32BigEndian(bytes.AsSpan(16)), Is.EqualTo(width));
                Assert.That(BinaryPrimitives.ReadInt32BigEndian(bytes.AsSpan(20)), Is.EqualTo(height));
                Assert.That(bytes[24], Is.EqualTo(8), "bit depth は 8");
                Assert.That(bytes[25], Is.EqualTo(2), "color type は truecolor");
            }
            finally
            {
                File.Delete(path);
            }
        }

        [Test]
        public void WriteBgra_ConvertsBgraToRgbInIdat()
        {
            // 1x1 画素。BGRA で B=0x10, G=0x20, R=0x30, A=0xFF → PNG では R,G,B の順
            var bgra = new byte[] { 0x10, 0x20, 0x30, 0xFF };
            var path = WriteTemporaryPng(bgra, 1, 1);
            try
            {
                var raw = InflateFirstIdat(File.ReadAllBytes(path));
                // 1 行 = フィルタバイト(0) + RGB 3 バイト
                Assert.That(raw, Is.EqualTo(new byte[] { 0x00, 0x30, 0x20, 0x10 }));
            }
            finally
            {
                File.Delete(path);
            }
        }

        [Test]
        public void WriteBgra_ThrowsWhenBufferIsTooSmall()
        {
            var path = Path.Combine(Path.GetTempPath(), $"png-writer-test-{Guid.NewGuid():N}.png");
            Assert.Throws<ArgumentException>(
                () => PortableNetworkGraphicsWriter.WriteBgra(path, new byte[4], 2, 2));
        }

        [Test]
        public void WriteBgra_ThrowsWhenDimensionsAreNotPositive()
        {
            var path = Path.Combine(Path.GetTempPath(), $"png-writer-test-{Guid.NewGuid():N}.png");
            Assert.Throws<ArgumentException>(
                () => PortableNetworkGraphicsWriter.WriteBgra(path, new byte[4], 0, 1));
        }

        /// <summary>最初の IDAT チャンクを取り出して zlib 展開する。</summary>
        private static byte[] InflateFirstIdat(byte[] png)
        {
            var offset = 8;
            while (offset < png.Length)
            {
                var length = BinaryPrimitives.ReadInt32BigEndian(png.AsSpan(offset));
                var type = Encoding.ASCII.GetString(png, offset + 4, 4);
                if (type == "IDAT")
                {
                    using var compressed = new MemoryStream(png, offset + 8, length);
                    using var inflate = new ZLibStream(compressed, CompressionMode.Decompress);
                    using var output = new MemoryStream();
                    inflate.CopyTo(output);
                    return output.ToArray();
                }
                offset += 12 + length; // length(4) + type(4) + data + crc(4)
            }
            throw new InvalidOperationException("IDAT chunk not found");
        }
    }
}
