// The world map the app's Map tab draws, read from the player's own game files:
//   <out>/map/map.json          world bounds + every resource site
//   <out>/map/<z>/<x>_<y>.png   the game's map art as a 4-level tile pyramid
// Nothing here touches the network; it only reads what the provider mounted.

using System.Text.Json;
using System.Text.RegularExpressions;
using CUE4Parse.FileProvider;
using CUE4Parse.UE4.Assets.Exports;
using CUE4Parse.UE4.Assets.Exports.Texture;
using CUE4Parse.UE4.Objects.Core.Math;
using CUE4Parse.UE4.Objects.UObject;
using CUE4Parse_Conversion.Textures;
using SkiaSharp;

static class MapExport
{
    public const int Levels = 4;
    public const int TilePx = 1024;
    /// One step for reading the level, then every tile (1 + 4 + 16 + 64).
    public const int Steps = 1 + 85;

    const string SlicedMap = "FactoryGame/Content/FactoryGame/Interface/UI/Assets/MapTest/SlicedMap/";
    const string LevelPath = "FactoryGame/Content/FactoryGame/Map/GameLevel01/Persistent_Level.umap";
    const int SourceTile = 4096, Grid = 2; // 2 x 2 source textures, 8192 px square
    static readonly Regex SafeName = new("^[A-Za-z0-9_]{1,128}$", RegexOptions.Compiled);

    sealed record Site(string Id, string Kind, string? Resource, string Purity, double X, double Y, double Z);

    /// <param name="step">Called once per finished unit of work (any thread, serialized).</param>
    public static void Run(AbstractFileProvider provider, string outDir, Action step)
    {
        var mapDir = Path.Combine(outDir, "map");
        var clock = System.Diagnostics.Stopwatch.StartNew();
        var (xMin, yMin, size, sites) = ReadLevel(provider);
        step();
        Console.WriteLine($"Map: {sites.Count} sites, x_min={xMin} y_min={yMin} size={size} ({clock.ElapsedMilliseconds} ms)");

        // Tiles first, map.json last: its presence means the whole export finished.
        Directory.CreateDirectory(mapDir);
        var mapJson = Path.Combine(mapDir, "map.json");
        if (File.Exists(mapJson)) File.Delete(mapJson);
        WriteTiles(provider, mapDir, step);

        var info = new
        {
            x_min = xMin, y_min = yMin, size, tile_px = TilePx, levels = Levels,
            nodes = sites.Select(s => new { id = s.Id, kind = s.Kind, resource = s.Resource, purity = s.Purity, x = s.X, y = s.Y, z = s.Z }),
        };
        File.WriteAllText(mapJson, JsonSerializer.Serialize(info));
        Console.WriteLine($"Wrote map -> {mapDir} ({clock.ElapsedMilliseconds} ms)");
    }

    static (double xMin, double yMin, double size, List<Site> sites) ReadLevel(AbstractFileProvider provider)
    {
        var pkg = provider.LoadPackage(LevelPath);
        double? orthoWidth = null;
        FVector? capturePos = null;
        var sites = new List<Site>();
        var seen = new HashSet<string>(StringComparer.Ordinal);

        foreach (var export in pkg.GetExports())
        {
            var type = export.ExportType;
            if (type == "BP_MinimapCaptureActor_C")
            {
                if (export.Name != "BP_MinimapCaptureActor2_2") continue;
                var cap = export.GetOrDefault<UObject?>("CaptureComponent2D")
                    ?? throw new InvalidDataException("minimap capture actor has no CaptureComponent2D");
                if (!cap.TryGetValue(out float ow, "OrthoWidth"))
                    throw new InvalidDataException("minimap capture component has no OrthoWidth");
                var scene = export.GetOrDefault<UObject?>("SceneComponent")
                    ?? throw new InvalidDataException("minimap capture actor has no SceneComponent");
                if (!scene.TryGetValue(out FVector loc, "RelativeLocation"))
                    throw new InvalidDataException("minimap capture SceneComponent has no RelativeLocation");
                orthoWidth = ow;
                capturePos = loc;
                continue;
            }
            var kind = type switch
            {
                "BP_ResourceNode_C" => "Node",
                "BP_ResourceNodeGeyser_C" => "Geyser",
                "BP_FrackingSatellite_C" => "WellSatellite",
                "BP_FrackingCore_C" => "WellCore",
                _ => null,
            };
            if (kind is null) continue;

            var id = export.Name;
            if (!SafeName.IsMatch(id)) throw new InvalidDataException($"unexpected site name '{id}'");
            if (!seen.Add(id)) throw new InvalidDataException($"duplicate site name '{id}'");

            // "RP_Inpure" (sic) / "RP_Pure"; absent means Normal.
            var purityRaw = export.TryGetValue(out FName p, "mPurity") ? p.Text : null;
            var purity = purityRaw switch
            {
                null => "Normal",
                "RP_Inpure" => "Impure",
                "RP_Normal" => "Normal",
                "RP_Pure" => "Pure",
                _ => throw new InvalidDataException($"{id}: unknown purity '{purityRaw}'"),
            };

            string? resource = null;
            if (export.TryGetValue(out FPackageIndex res, "mResourceClass") && !res.IsNull)
            {
                resource = res.Name;
                if (!SafeName.IsMatch(resource)) throw new InvalidDataException($"{id}: unexpected resource '{resource}'");
            }
            if (kind != "Geyser" && resource is null) throw new InvalidDataException($"{id}: no mResourceClass");

            var root = export.GetOrDefault<UObject?>("RootComponent")
                ?? throw new InvalidDataException($"{id}: no RootComponent");
            if (!root.TryGetValue(out FVector pos, "RelativeLocation"))
                throw new InvalidDataException($"{id}: root component has no RelativeLocation");
            if (root.TryGetValue(out FPackageIndex parent, "AttachParent") && !parent.IsNull)
                throw new InvalidDataException($"{id}: root component is attached; its position is not in world space");
            if (!double.IsFinite(pos.X) || !double.IsFinite(pos.Y) || !double.IsFinite(pos.Z))
                throw new InvalidDataException($"{id}: bad position");
            sites.Add(new Site(id, kind, resource, purity, pos.X, pos.Y, pos.Z));
        }

        if (orthoWidth is not { } w || capturePos is not { } c)
            throw new InvalidDataException("BP_MinimapCaptureActor2_2 not found in the level: the map's world bounds are unknown");
        if (!(w > 0) || !double.IsFinite(w) || !double.IsFinite(c.X) || !double.IsFinite(c.Y))
            throw new InvalidDataException("bad map bounds");
        if (!sites.Any(s => s.Kind == "Node"))
            throw new InvalidDataException("no resource nodes found in the level");
        sites.Sort((a, b) => string.CompareOrdinal(a.Id, b.Id));
        return (c.X - w / 2, c.Y - w / 2, w, sites);
    }

    static void WriteTiles(AbstractFileProvider provider, string mapDir, Action step)
    {
        var full = SourceTile * Grid;
        var info = new SKImageInfo(full, full, SKColorType.Rgba8888, SKAlphaType.Unpremul);
        var current = new SKBitmap(info);
        using (var canvas = new SKCanvas(current))
        using (var paint = new SKPaint { BlendMode = SKBlendMode.Src })
        {
            for (var col = 0; col < Grid; col++)
                for (var row = 0; row < Grid; row++)
                {
                    var name = $"Map_{col}-{row}";
                    var tex = provider.LoadPackageObject<UTexture2D>($"{SlicedMap}{name}.{name}");
                    var decoded = TextureDecoder.Decode(tex, ETexturePlatform.DesktopMobile)
                        ?? throw new InvalidDataException($"cannot decode {name}");
                    using var bmp = TextureEncoder.ToSkBitmap(decoded);
                    if (bmp.Width != SourceTile || bmp.Height != SourceTile)
                        throw new InvalidDataException($"{name} is {bmp.Width}x{bmp.Height}, expected {SourceTile}");
                    canvas.DrawBitmap(bmp, col * SourceTile, row * SourceTile, paint);
                }
        }

        var gate = new object();
        // Level 3 is the full image; each level below is half of the one above.
        for (var z = Levels - 1; z >= 0; z--)
        {
            var n = 1 << z;
            if (current.Width != TilePx * n) throw new InvalidOperationException("tile pyramid out of step");
            var dir = Path.Combine(mapDir, z.ToString());
            if (Directory.Exists(dir)) Directory.Delete(dir, true);
            Directory.CreateDirectory(dir);
            var level = current;
            Parallel.For(0, n * n, i =>
            {
                int tx = i % n, ty = i / n;
                using var tile = new SKBitmap();
                if (!level.ExtractSubset(tile, new SKRectI(tx * TilePx, ty * TilePx, (tx + 1) * TilePx, (ty + 1) * TilePx)))
                    throw new InvalidOperationException("tile crop failed");
                using var img = SKImage.FromBitmap(tile);
                using var data = img.Encode(SKEncodedImageFormat.Png, PngQuality);
                File.WriteAllBytes(Path.Combine(dir, $"{tx}_{ty}.png"), data.ToArray());
                lock (gate) step();
            });
            if (z > 0)
            {
                var half = TilePx * (n / 2);
                var smaller = current.Resize(new SKImageInfo(half, half, SKColorType.Rgba8888, SKAlphaType.Unpremul), SKFilterQuality.High)
                    ?? throw new InvalidOperationException("downscale failed");
                current.Dispose();
                current = smaller;
            }
        }
        current.Dispose();
    }

    // Skia maps this to the PNG zlib level; chosen by measurement.
    const int PngQuality = 100;
}
