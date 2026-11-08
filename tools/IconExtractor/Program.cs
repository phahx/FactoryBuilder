// Extracts the item/building icons Factory Builder shows from the player's own
// Satisfactory install into a local cache folder. Nothing extracted here is
// redistributed: the app reads the cache on the machine it was extracted on.
//
// Usage:
//   IconExtractor --game "<Satisfactory folder>" --out "<output folder>"
//                 [--oodle <decoder dll>] [--engine GAME_UE5_6] [--size 128] [--progress]
//
// The game's files are Oodle-compressed. By default the extractor uses
// oodle_shim.dll from its own folder (crates/oodle-shim: an open-source
// decoder that exports Oodle's decompress function). --oodle picks another
// DLL with the same export. Nothing is searched for or downloaded.
//
// --progress prints machine-readable lines for the app's progress bar:
//   @stage <name>          mount | icons | ui | map | sprites
//   @progress <done> <total>

using System.Text;
using System.Text.Json;
using CUE4Parse.Compression;
using CUE4Parse.FileProvider;
using CUE4Parse.MappingsProvider.Usmap;
using CUE4Parse.UE4.Assets.Exports.Texture;
using CUE4Parse.UE4.Versions;
using CUE4Parse_Conversion.Options;
using CUE4Parse_Conversion.Textures;

static string? Arg(string[] args, string name)
{
    var i = Array.IndexOf(args, name);
    return i >= 0 && i + 1 < args.Length ? args[i + 1] : null;
}

var game = Arg(args, "--game");
var oodle = Arg(args, "--oodle") ?? Path.Combine(AppContext.BaseDirectory, "oodle_shim.dll");
var outDir = Arg(args, "--out");
var engine = Arg(args, "--engine") ?? "GAME_UE5_6";
var size = int.Parse(Arg(args, "--size") ?? "128");
var progress = args.Contains("--progress");
void Stage(string name) { if (progress) Console.WriteLine($"@stage {name}"); }
void Progress(int done, int total) { if (progress) Console.WriteLine($"@progress {done} {total}"); }

if (game is null || outDir is null)
{
    Console.Error.WriteLine("usage: IconExtractor --game <dir> --out <dir> [--oodle <dll>] [--engine GAME_UE5_6] [--size 128] [--progress]");
    return 2;
}
if (!File.Exists(oodle))
{
    Console.Error.WriteLine($"Oodle DLL not found: {oodle}");
    return 2;
}

var docsPath = Path.Combine(game, "CommunityResources", "Docs", "en-US.json");
var usmapPath = Path.Combine(game, "CommunityResources", "FactoryGame.usmap");
var paksDir = Path.Combine(game, "FactoryGame", "Content", "Paks");
foreach (var p in new[] { docsPath, usmapPath, paksDir })
{
    if (!File.Exists(p) && !Directory.Exists(p))
    {
        Console.Error.WriteLine($"Missing game file: {p}");
        return 2;
    }
}

// 1. Collect ClassName -> texture object path from the game's Docs JSON.
//    Docs.json is UTF-16 LE with a BOM; File.ReadAllText detects that.
var icons = new SortedDictionary<string, string>(StringComparer.Ordinal);
using (var doc = JsonDocument.Parse(File.ReadAllText(docsPath, Encoding.Unicode)))
{
    foreach (var native in doc.RootElement.EnumerateArray())
    {
        foreach (var cls in native.GetProperty("Classes").EnumerateArray())
        {
            if (!cls.TryGetProperty("ClassName", out var nameEl)) continue;
            string? tex = null;
            foreach (var key in new[] { "mPersistentBigIcon", "mSmallIcon" })
            {
                if (cls.TryGetProperty(key, out var v) && v.GetString() is { } s && s.StartsWith("Texture2D "))
                {
                    tex = s["Texture2D ".Length..].Trim();
                    break;
                }
            }
            if (tex is not null) icons[nameEl.GetString()!] = tex;
        }
    }
}
Console.WriteLine($"Docs list {icons.Count} classes with icons");

// 2. Mount the game's IoStore containers.
Stage("mount");
OodleHelper.Initialize(oodle);
var version = new VersionContainer(Enum.Parse<EGame>(engine));
var fixedUsmap = Path.Combine(Path.GetTempPath(), "FactoryBuilder-FactoryGame.usmap");
File.WriteAllBytes(fixedUsmap, UsmapFix.Patch(File.ReadAllBytes(usmapPath)));
var provider = new DefaultFileProvider(paksDir, SearchOption.TopDirectoryOnly, version, StringComparer.OrdinalIgnoreCase)
{
    MappingsContainer = new FileUsmapTypeMappingsProvider(fixedUsmap, StringComparer.OrdinalIgnoreCase),
};
provider.Initialize();
provider.Mount();
Console.WriteLine($"Mounted {provider.MountedVfs.Count} containers, {provider.Files.Count} files");

// Exploration helpers (used to study the game's own UI):
//   --list <substring>     print matching package paths
//   --dump <path>          write a texture as PNG, or a .ufont as-is, into --out
//   --json <path>          write the package's exports as JSON into --out
if (Arg(args, "--list") is { } needle)
{
    foreach (var f in provider.Files.Keys.Where(k => k.Contains(needle, StringComparison.OrdinalIgnoreCase)).Order())
        Console.WriteLine(f);
    return 0;
}
if (Arg(args, "--dump") is { } dumpPath)
{
    Directory.CreateDirectory(outDir);
    var name = Path.GetFileNameWithoutExtension(dumpPath);
    if (dumpPath.EndsWith(".ufont", StringComparison.OrdinalIgnoreCase))
    {
        File.WriteAllBytes(Path.Combine(outDir, name + ".ttf"), provider.SaveAsset(dumpPath));
    }
    else
    {
        var obj = provider.LoadPackageObject<UTexture2D>(Path.ChangeExtension(dumpPath, null) + "." + name);
        var tex = TextureDecoder.Decode(obj, ETexturePlatform.DesktopMobile)!;
        File.WriteAllBytes(Path.Combine(outDir, name + ".png"), TextureEncoder.Encode(tex, ETextureFormat.Png, false, out _, 100, 6, false));
    }
    Console.WriteLine($"dumped {name}");
    return 0;
}
//   --app-icon <Class>     write icon.ico (256..16 px) and icon.png (256 px) of that
//                          item/building icon into --out (the app's window/exe icon)
if (Arg(args, "--app-icon") is { } iconClass)
{
    if (!icons.TryGetValue(iconClass, out var texPath))
    {
        Console.Error.WriteLine($"No icon for {iconClass} in Docs");
        return 2;
    }
    Directory.CreateDirectory(outDir);
    var decoded = TextureDecoder.Decode(provider.LoadPackageObject<UTexture2D>(texPath), 256, ETexturePlatform.DesktopMobile)!;
    using var full = TextureEncoder.ToSkBitmap(decoded);
    int[] sizes = [256, 128, 64, 48, 32, 24, 16];
    var pngs = sizes.Select(s =>
    {
        using var scaled = full.Width == s ? full.Copy() : full.Resize(new SkiaSharp.SKImageInfo(s, s, SkiaSharp.SKColorType.Rgba8888, SkiaSharp.SKAlphaType.Unpremul), SkiaSharp.SKFilterQuality.High);
        using var img = SkiaSharp.SKImage.FromBitmap(scaled);
        return img.Encode(SkiaSharp.SKEncodedImageFormat.Png, 100).ToArray();
    }).ToArray();
    File.WriteAllBytes(Path.Combine(outDir, "icon.png"), pngs[0]);
    // ICO with PNG-compressed entries (supported since Windows Vista).
    using var ico = new MemoryStream();
    using (var w = new BinaryWriter(ico, Encoding.UTF8, leaveOpen: true))
    {
        w.Write((ushort)0); w.Write((ushort)1); w.Write((ushort)sizes.Length);
        var offset = 6 + 16 * sizes.Length;
        for (var i = 0; i < sizes.Length; i++)
        {
            w.Write((byte)(sizes[i] % 256)); w.Write((byte)(sizes[i] % 256));
            w.Write((byte)0); w.Write((byte)0);
            w.Write((ushort)1); w.Write((ushort)32);
            w.Write(pngs[i].Length); w.Write(offset);
            offset += pngs[i].Length;
        }
        foreach (var p in pngs) w.Write(p);
    }
    File.WriteAllBytes(Path.Combine(outDir, "icon.ico"), ico.ToArray());
    Console.WriteLine($"app icon from {iconClass} ({full.Width}px source) -> {outDir}");
    return 0;
}
if (Arg(args, "--locres") is { } locPath)
{
    Directory.CreateDirectory(outDir);
    var res = new CUE4Parse.UE4.Localization.FTextLocalizationResource(
        new CUE4Parse.UE4.Readers.FByteArchive(locPath, provider.SaveAsset(locPath)));
    var lines = res.Entries.SelectMany(ns => ns.Value.Select(kv => $"{ns.Key.Str}\t{kv.Key.Str}\t{kv.Value.LocalizedString.Replace("\r", "").Replace("\n", "\\n")}"));
    File.WriteAllLines(Path.Combine(outDir, "locres.tsv"), lines);
    Console.WriteLine("locres written");
    return 0;
}
//   --sprites-only [Class] render only the map sprites (or one class) into --out
if (args.Contains("--sprites-only"))
{
    var one = Arg(args, "--sprites-only") is { } c && !c.StartsWith("--") ? c : null;
    SpriteExport.Run(provider, SpriteExport.ListJobs(docsPath), outDir, () => { }, one);
    return 0;
}
if (Arg(args, "--json") is { } jsonPath)
{
    Directory.CreateDirectory(outDir);
    var pkg = provider.LoadPackage(jsonPath);
    var json = Newtonsoft.Json.JsonConvert.SerializeObject(pkg.GetExports(), Newtonsoft.Json.Formatting.Indented);
    File.WriteAllText(Path.Combine(outDir, Path.GetFileNameWithoutExtension(jsonPath) + ".json"), json);
    Console.WriteLine($"json {jsonPath}");
    return 0;
}

// 3. Decode each unique texture once and write <ClassName>.png.
Directory.CreateDirectory(outDir);
Stage("icons");
const int UiCount = 17; // fonts + UI textures below, for one progress total
var spriteJobs = SpriteExport.ListJobs(docsPath);
int total = icons.Count + UiCount + MapExport.Steps + spriteJobs.Count, step = 0;
Progress(0, total);
var cache = new Dictionary<string, byte[]?>(StringComparer.OrdinalIgnoreCase);
var manifest = new SortedDictionary<string, string>(StringComparer.Ordinal);
int ok = 0, failed = 0;
foreach (var (cls, tex) in icons)
{
    if (!cache.TryGetValue(tex, out var png))
    {
        png = null;
        try
        {
            var texture = provider.LoadPackageObject<UTexture2D>(tex);
            var decoded = TextureDecoder.Decode(texture, size, ETexturePlatform.DesktopMobile);
            if (decoded is not null)
                png = TextureEncoder.Encode(decoded, ETextureFormat.Png, false, out _, 100, 6, false);
        }
        catch (Exception e)
        {
            Console.Error.WriteLine($"  {cls}: {tex}: {e.GetType().Name}: {e.Message}");
        }
        cache[tex] = png;
    }
    if (++step % 8 == 0) Progress(step, total);
    if (png is null) { failed++; continue; }
    var file = cls + ".png";
    File.WriteAllBytes(Path.Combine(outDir, file), png);
    manifest[cls] = file;
    ok++;
}

File.WriteAllText(Path.Combine(outDir, "manifest.json"),
    JsonSerializer.Serialize(manifest, new JsonSerializerOptions { WriteIndented = true }));
Console.WriteLine($"Wrote {ok} icons ({cache.Count} unique textures), {failed} failed -> {outDir}");

// 4. The game's own UI font and UI icons, so the app can look like the game.
Stage("ui");
var uiDir = Path.Combine(outDir, "ui");
Directory.CreateDirectory(uiDir);
const string Ui = "FactoryGame/Content/FactoryGame/Interface/";
string[] fonts = ["OpenSans-Regular", "OpenSans-SemiBold", "OpenSans-Bold"];
string[] uiTextures =
[
    "UI/Assets/MonochromeIcons/TXUI_MIcon_Trash", "UI/Assets/MonochromeIcons/TXUI_MIcon_Cogwheel",
    "UI/Assets/MonochromeIcons/TXUI_MIcon_Warning", "UI/Assets/MonochromeIcons/TXUI_MIcon_Power",
    "UI/Assets/MonochromeIcons/TXUI_MIcon_Factory", "UI/Assets/MonochromeIcons/TXUI_MIcon_Stop_X",
    "UI/Assets/MonochromeIcons/TXUI_MIcon_Recycle", "UI/Assets/MonochromeIcons/TXUI_MIcon_Arrow_Right",
    "UI/Assets/Shared/SearchIcon", "UI/Assets/Shared/Reset_Icon", "UI/Assets/Shared/TXUI_Icon_Copy",
    "UI/Assets/Shared/ficsit_checkmark_256", "UI/Assets/BuildMenu/TXUI_AddTolist_32", "UI/Menu/Graphics/Caret",
];
int uiOk = 0;
if (fonts.Length + uiTextures.Length != UiCount) throw new InvalidOperationException("UiCount is out of date");
foreach (var f in fonts)
{
    try
    {
        // Cooked .ufont = int32 length + the font file (+ padding).
        var raw = provider.SaveAsset($"{Ui}Font/{f}.ufont");
        var len = BitConverter.ToInt32(raw, 0);
        var ttf = raw.AsSpan(4, len).ToArray();
        if (BitConverter.ToUInt32(ttf, 0) != 0x00000100) throw new InvalidDataException("not a TrueType font");
        File.WriteAllBytes(Path.Combine(uiDir, f + ".ttf"), ttf);
        uiOk++;
    }
    catch (Exception e) { Console.Error.WriteLine($"  font {f}: {e.Message}"); }
    Progress(++step, total);
}
foreach (var t in uiTextures)
{
    var name = Path.GetFileName(t);
    try
    {
        var texture = provider.LoadPackageObject<UTexture2D>($"{Ui}{t}.{name}");
        var decoded = TextureDecoder.Decode(texture, 128, ETexturePlatform.DesktopMobile)!;
        File.WriteAllBytes(Path.Combine(uiDir, name + ".png"), TextureEncoder.Encode(decoded, ETextureFormat.Png, false, out _, 100, 6, false));
        uiOk++;
    }
    catch (Exception e) { Console.Error.WriteLine($"  ui {name}: {e.Message}"); }
    Progress(++step, total);
}
Console.WriteLine($"Wrote {uiOk}/{fonts.Length + uiTextures.Length} UI assets -> {uiDir}");

// 5. The world map: tiles from the game's map art, resource sites and world bounds.
Stage("map");
var mapOk = false;
try
{
    MapExport.Run(provider, outDir, () => Progress(++step, total));
    mapOk = true;
}
catch (Exception e) { Console.Error.WriteLine($"  map: {e.GetType().Name}: {e.Message}"); }

// 6. Top-down renders of the buildings, belts and pipes for the map. Optional:
//    without them the map falls back to icons, so a failure here doesn't fail
//    the extraction (it is reported on stderr).
Stage("sprites");
try
{
    SpriteExport.Run(provider, spriteJobs, outDir, () => Progress(++step, total));
}
catch (Exception e) { Console.Error.WriteLine($"  sprites: {e.GetType().Name}: {e.Message}"); }
Progress(total, total);
return ok > 0 && mapOk ? 0 : 1;
