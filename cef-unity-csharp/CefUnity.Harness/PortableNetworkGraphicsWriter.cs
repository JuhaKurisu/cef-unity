using System;
using System.Buffers.Binary;
using System.IO;
using System.IO.Compression;

namespace CefUnity.Harness
{

/// <summary>
///     BGRA8 バッファを PNG として書き出す最小エンコーダ (診断専用)。
///     フィルタは None、色型は Truecolor (RGB 8bit) を使う。
/// </summary>
public static class PortableNetworkGraphicsWriter
{
    private static readonly byte[] Signature = { 0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A };

    public static void WriteBgra(string path, byte[] bgra, int width, int height)
    {
        if (width <= 0 || height <= 0)
            throw new ArgumentException($"invalid dimensions: {width}x{height}");
        var expected = (long)width * height * 4;
        if (bgra.Length < expected)
            throw new ArgumentException($"buffer too small: {bgra.Length} < {expected}");

        // フィルタバイト (0 = None) + RGB 3 バイト/画素 を行ごとに並べる
        var raw = new byte[(1 + (long)width * 3) * height];
        var rawIndex = 0;
        for (var y = 0; y < height; y++)
        {
            raw[rawIndex++] = 0;
            var rowStart = (long)y * width * 4;
            for (var x = 0; x < width; x++)
            {
                var pixel = rowStart + (long)x * 4;
                raw[rawIndex++] = bgra[pixel + 2]; // R
                raw[rawIndex++] = bgra[pixel + 1]; // G
                raw[rawIndex++] = bgra[pixel + 0]; // B
            }
        }

        using var compressed = new MemoryStream();
        using (var deflate = new ZLibStream(compressed, CompressionLevel.Optimal, leaveOpen: true))
        {
            deflate.Write(raw, 0, raw.Length);
        }

        using var file = File.Create(path);
        file.Write(Signature);

        var header = new byte[13];
        BinaryPrimitives.WriteInt32BigEndian(header.AsSpan(0), width);
        BinaryPrimitives.WriteInt32BigEndian(header.AsSpan(4), height);
        header[8] = 8;  // bit depth
        header[9] = 2;  // color type: truecolor
        header[10] = 0; // compression: deflate
        header[11] = 0; // filter: adaptive
        header[12] = 0; // interlace: none
        WriteChunk(file, "IHDR", header);
        WriteChunk(file, "IDAT", compressed.ToArray());
        WriteChunk(file, "IEND", Array.Empty<byte>());
    }

    private static void WriteChunk(Stream stream, string type, byte[] data)
    {
        var length = new byte[4];
        BinaryPrimitives.WriteInt32BigEndian(length, data.Length);
        stream.Write(length);

        var typeBytes = new byte[4];
        for (var index = 0; index < 4; index++) typeBytes[index] = (byte)type[index];
        stream.Write(typeBytes);
        stream.Write(data);

        var crc = ComputeCyclicRedundancyCheck(typeBytes, data);
        var crcBytes = new byte[4];
        BinaryPrimitives.WriteUInt32BigEndian(crcBytes, crc);
        stream.Write(crcBytes);
    }

    private static readonly uint[] CrcTable = BuildCrcTable();

    private static uint[] BuildCrcTable()
    {
        var table = new uint[256];
        for (var index = 0u; index < 256; index++)
        {
            var value = index;
            for (var bit = 0; bit < 8; bit++)
                value = (value & 1) != 0 ? 0xEDB88320u ^ (value >> 1) : value >> 1;
            table[index] = value;
        }
        return table;
    }

    private static uint ComputeCyclicRedundancyCheck(byte[] type, byte[] data)
    {
        var crc = 0xFFFFFFFFu;
        foreach (var value in type) crc = CrcTable[(crc ^ value) & 0xFF] ^ (crc >> 8);
        foreach (var value in data) crc = CrcTable[(crc ^ value) & 0xFF] ^ (crc >> 8);
        return crc ^ 0xFFFFFFFFu;
    }
}

}
