using System.Buffers.Binary;

/// The FactoryGame.usmap shipped in CommunityResources writes OptionalProperty
/// (type 28) as a leaf, with no inner type. CUE4Parse reads an inner type after
/// it, so the struct table goes out of alignment and parsing throws.
/// This copies the file, adds a ByteProperty inner type after every Optional,
/// and fixes the two body-size fields in the header. Only uncompressed
/// usmap files (what the game ships) are handled.
static class UsmapFix
{
    const byte Byte = 0, Array = 8, Struct = 9, Map = 24, Set = 25, Enum = 26, Optional = 28;

    public static byte[] Patch(byte[] src)
    {
        int o = 0;
        ushort U16() { var v = BinaryPrimitives.ReadUInt16LittleEndian(src.AsSpan(o)); o += 2; return v; }
        int I32() { var v = BinaryPrimitives.ReadInt32LittleEndian(src.AsSpan(o)); o += 4; return v; }
        byte U8() => src[o++];

        if (U16() != 0x30C4) throw new InvalidDataException("not a usmap file");
        var version = U8();
        if (version >= 1 && I32() != 0)
        {
            o += 8;                 // UE4 + UE5 file version
            var customs = I32();
            o += customs * 20;      // custom versions (guid + int)
            o += 4;                 // net CL
        }
        var compressionOffset = o;
        if (U8() != 0) throw new InvalidDataException("compressed usmap files are not supported");
        o += 8;                     // compressed + decompressed size
        var bodyStart = o;

        var nameLen32 = version >= 2; // LongFName
        var names = I32();
        for (var i = 0; i < names; i++)
        {
            int len = nameLen32 ? U16() : U8(); // read before `o +=` captures o
            o += len;
        }

        var enums = I32();
        var largeEnums = version >= 3;
        for (var i = 0; i < enums; i++)
        {
            o += 4;
            var count = largeEnums ? U16() : U8();
            var entrySize = version >= 4 ? 8 + 4 : 4;
            o += count * entrySize;
        }

        var inserts = new List<int>();
        void PropType()
        {
            var t = U8();
            switch (t)
            {
                case Enum: PropType(); o += 4; break;
                case Struct: o += 4; break;
                case Array or Set: PropType(); break;
                case Map: PropType(); PropType(); break;
                case Optional: inserts.Add(o); break; // inner type missing here
            }
        }

        var structs = I32();
        for (var i = 0; i < structs; i++)
        {
            o += 8;                  // name + super
            o += 2;                  // property count
            var serializable = U16();
            for (var p = 0; p < serializable; p++)
            {
                o += 2 + 1 + 4;      // schema index, array size, name
                PropType();
            }
        }

        if (inserts.Count == 0) return src;
        var dst = new byte[src.Length + inserts.Count];
        int from = 0, to = 0;
        foreach (var at in inserts)
        {
            var len = at - from;
            System.Array.Copy(src, from, dst, to, len);
            to += len; from = at;
            dst[to++] = Byte;
        }
        System.Array.Copy(src, from, dst, to, src.Length - from);

        var bodySize = (uint)(dst.Length - bodyStart);
        BinaryPrimitives.WriteUInt32LittleEndian(dst.AsSpan(compressionOffset + 1), bodySize);
        BinaryPrimitives.WriteUInt32LittleEndian(dst.AsSpan(compressionOffset + 5), bodySize);
        return dst;
    }
}
