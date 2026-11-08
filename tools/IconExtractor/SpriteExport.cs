// Top-down renders of the game's own building models for the app's Map tab:
//   <out>/map/sprites/<Class>.png     one image per building / belt / pipe class
//   <out>/map/sprites/sprites.json    fb_model::world::Sprites (written last)
//
// Buildings are drawn as seen from straight above (orthographic, highest
// surface wins), in the building's local space: image right = local +X,
// image down = local +Y (Unreal axes), at a fixed CmPerPx for every class.
// Belts and pipes get one straight piece of the mesh their splines repeat:
// image right = along the belt, image down = across it.
//
// Meshes come from each class's Blueprint: the SimpleConstructionScript's
// static mesh component templates (with inherited ones and their overrides),
// the CDO's abstract instance data (mInstanceDataCDO), and for belts, pipes
// and lifts the meshes their CDO names. Only LOD0 is drawn; decals, see-through
// materials and LOD stand-in materials are left out, and so are components
// attached to a socket or bone (their place depends on a skeleton).
//
// Colours: the factory materials' own textures (MM_FactoryBaked: albedo, AO in
// AOMasks.R, primary paint mask in AOMasks.G; MM_Factory_Array:
// one texture-array slice per UV0 third, two of them painted), painted with the
// game's default FICSIT primary/secondary colours from BP_GameState's
// mStaticGlobalColorPresets. Anything else is its albedo texture or neutral grey.
// Lambert shading with a light from above (and a little from the top-left).
//
// Nothing here touches the network; it only reads what the provider mounted.

using System.Collections.Concurrent;
using System.Numerics;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;
using CUE4Parse.FileProvider;
using CUE4Parse.UE4.Assets.Exports;
using CUE4Parse.UE4.Assets.Exports.Material;
using CUE4Parse.UE4.Assets.Exports.StaticMesh;
using CUE4Parse.UE4.Assets.Exports.Texture;
using CUE4Parse.UE4.Assets.Objects;
using CUE4Parse.UE4.Objects.Core.i18N;
using CUE4Parse.UE4.Objects.Core.Math;
using CUE4Parse.UE4.Objects.Engine;
using CUE4Parse.UE4.Objects.UObject;
using CUE4Parse_Conversion.Textures;
using SkiaSharp;

static class SpriteExport
{
    /// Render resolution for every building, belt and pipe.
    public const float CmPerPx = 4f;
    /// Samples per pixel side (fewer for very large buildings, see SamplesFor).
    const int Supersample = 3;
    /// Most supersampled samples one image may use (memory bound).
    const long MaxSamples = 24_000_000;
    /// Largest image side; anything bigger is not a building we expect.
    const int MaxSidePx = 4096;

    static readonly Regex SafeName = new("^[A-Za-z0-9_]{1,128}$", RegexOptions.Compiled);

    /// Docs native classes whose buildings get a sprite.
    static readonly string[] BuildingNatives =
    [
        "FGBuildableManufacturer", "FGBuildableManufacturerVariablePower",
        "FGBuildableResourceExtractor", "FGBuildableWaterPump", "FGBuildableFrackingExtractor", "FGBuildableFrackingActivator",
        "FGBuildableGeneratorFuel", "FGBuildableGeneratorNuclear", "FGBuildableGeneratorGeoThermal",
        "FGBuildableAttachmentSplitter", "FGBuildableAttachmentMerger", "FGBuildableSplitterSmart", "FGBuildableMergerPriority",
        "FGBuildableStorage", "FGCentralStorageContainer", "FGBuildablePipeReservoir",
        "FGBuildablePipelineJunction", "FGBuildablePipelinePump",
        "FGBuildableTrainPlatformCargo", "FGBuildableRailroadStation", "FGBuildableDockingStation", "FGBuildableDroneStation",
        "FGBuildableResourceSink", "FGBuildableSpaceElevator",
        "FGBuildableConveyorLift",
        "FGBuildableTradingPost",
        "FGBuildablePowerPole", "FGBuildablePowerStorage", "FGBuildableCircuitSwitch", "FGBuildablePriorityPowerSwitch",
    ];
    /// Docs native classes drawn as a repeated straight piece.
    static readonly string[] StripNatives = ["FGBuildableConveyorBelt", "FGBuildablePipeline"];

    public sealed record Job(string Class, string Native, string? FullName, bool Strip, float MeshLength);

    /// The classes to render, from the game's Docs (UTF-16 JSON).
    public static List<Job> ListJobs(string docsPath)
    {
        var jobs = new List<Job>();
        using var doc = JsonDocument.Parse(File.ReadAllText(docsPath, Encoding.Unicode));
        foreach (var native in doc.RootElement.EnumerateArray())
        {
            var nc = native.TryGetProperty("NativeClass", out var ncEl) ? ncEl.GetString() ?? "" : "";
            var m = Regex.Match(nc, @"\.(FG\w+)'?$");
            if (!m.Success) continue;
            var nativeName = m.Groups[1].Value;
            var strip = StripNatives.Contains(nativeName);
            if (!strip && !BuildingNatives.Contains(nativeName)) continue;
            foreach (var cls in native.GetProperty("Classes").EnumerateArray())
            {
                if (!cls.TryGetProperty("ClassName", out var nameEl) || nameEl.GetString() is not { } name || !SafeName.IsMatch(name)) continue;
                var full = cls.TryGetProperty("FullName", out var f) ? f.GetString() : null;
                float len = 0;
                if (cls.TryGetProperty("mMeshLength", out var l) && float.TryParse(l.GetString(), System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out var v))
                    len = v;
                jobs.Add(new Job(name, nativeName, full, strip, len));
            }
        }
        jobs.Sort((a, b) => string.CompareOrdinal(a.Class, b.Class));
        return jobs;
    }

    /// <param name="step">Called once per finished class (any thread, serialized).</param>
    public static void Run(AbstractFileProvider provider, List<Job> jobs, string outDir, Action step, string? only = null)
    {
        var clock = System.Diagnostics.Stopwatch.StartNew();
        var dir = Path.Combine(outDir, "map", "sprites");
        if (Directory.Exists(dir)) Directory.Delete(dir, true);
        Directory.CreateDirectory(dir);

        var r = new Renderer(provider);
        r.LoadPaint();
        var buildings = new ConcurrentDictionary<string, object>();
        var strips = new ConcurrentDictionary<string, object>();
        var gate = new object();
        long bytes = 0;
        var selected = only is null ? jobs : jobs.Where(j => j.Class == only).ToList();
        Parallel.ForEach(selected, new ParallelOptions { MaxDegreeOfParallelism = Environment.ProcessorCount }, job =>
        {
            try
            {
                var file = job.Class + ".png";
                if (job.Strip)
                {
                    var (png, widthCm, length) = r.RenderStrip(job);
                    File.WriteAllBytes(Path.Combine(dir, file), png);
                    Interlocked.Add(ref bytes, png.Length);
                    strips[job.Class] = new { file, length_cm = length, width_cm = widthCm };
                }
                else
                {
                    var (png, min, max) = r.RenderBuilding(job);
                    File.WriteAllBytes(Path.Combine(dir, file), png);
                    Interlocked.Add(ref bytes, png.Length);
                    buildings[job.Class] = new { file, min_x = min.X, min_y = min.Y, max_x = max.X, max_y = max.Y };
                }
            }
            catch (Exception e)
            {
                Console.Error.WriteLine($"  sprite {job.Class}: {e.GetType().Name}: {e.Message}");
            }
            lock (gate) step();
        });

        var info = new
        {
            cm_per_px = CmPerPx,
            buildings = new SortedDictionary<string, object>(buildings, StringComparer.Ordinal),
            strips = new SortedDictionary<string, object>(strips, StringComparer.Ordinal),
        };
        File.WriteAllText(Path.Combine(dir, "sprites.json"), JsonSerializer.Serialize(info));
        Console.WriteLine($"Wrote {buildings.Count} building sprites and {strips.Count} strips ({bytes / 1024} KB) -> {dir} ({clock.ElapsedMilliseconds} ms)");
        foreach (var line in r.Notes.OrderBy(s => s, StringComparer.Ordinal)) Console.WriteLine("  " + line);
    }

    // ------------------------------------------------------------- geometry

    /// An affine map p -> X*p.x + Y*p.y + Z*p.z + T.
    readonly record struct Affine(Vector3 X, Vector3 Y, Vector3 Z, Vector3 T)
    {
        public static readonly Affine Identity = new(Vector3.UnitX, Vector3.UnitY, Vector3.UnitZ, Vector3.Zero);
        public Vector3 Apply(Vector3 p) => X * p.X + Y * p.Y + Z * p.Z + T;
        public Vector3 Linear(Vector3 p) => X * p.X + Y * p.Y + Z * p.Z;
        /// this ∘ inner: apply inner first.
        public Affine Then(Affine inner) => new(Linear(inner.X), Linear(inner.Y), Linear(inner.Z), Apply(inner.T));

        /// Unreal's FTransform order: scale, then rotate, then translate.
        public static Affine From(FQuat rot, FVector pos, FVector scale)
        {
            Vector3 F(FVector v) { var s = new FVector(v.X * scale.X, v.Y * scale.Y, v.Z * scale.Z); var r = rot.RotateVector(s); return new Vector3(r.X, r.Y, r.Z); }
            var t = new Vector3(pos.X, pos.Y, pos.Z);
            return new Affine(F(new FVector(1, 0, 0)), F(new FVector(0, 1, 0)), F(new FVector(0, 0, 1)), t);
        }
    }

    sealed record Part(string Mesh, Affine Xf, IReadOnlyList<FPackageIndex?> Overrides, string Source);

    sealed class MeshData
    {
        public required string Name;
        public required Vector3[] Pos;
        public required Vector2[] Uv0;
        public required uint[] Idx;
        public required (int Mat, int First, int Tris)[] Sections;
        public required FPackageIndex?[] Materials;
    }

    enum Kind { Skip, Array, Baked, Other }

    sealed class MatInfo
    {
        public Kind Kind;
        public string Root = "";
        public Tex? Albedo;
        public Tex? Masks;
        public Vector3 Flat = new(0.214f); // sRGB 128, neutral grey
        public string Blend = "", Domain = "";
        /// A see-through surface (glass, a belt's glowing strip): skipped on buildings.
        public bool Translucent;
    }

    /// Decoded texture: linear RGB (from sRGB when Srgb) and raw channels.
    sealed class Tex
    {
        public int W, H;
        public required byte[] Rgba;
        public bool Srgb;
        public Vector4 Sample(Vector2 uv)
        {
            var x = (int)Math.Floor((uv.X - Math.Floor(uv.X)) * W);
            var y = (int)Math.Floor((uv.Y - Math.Floor(uv.Y)) * H);
            x = Math.Clamp(x, 0, W - 1); y = Math.Clamp(y, 0, H - 1);
            var i = (y * W + x) * 4;
            return new Vector4(Rgba[i], Rgba[i + 1], Rgba[i + 2], Rgba[i + 3]) / 255f;
        }
    }

    static float ToLinear(float c) => c <= 0.04045f ? c / 12.92f : MathF.Pow((c + 0.055f) / 1.055f, 2.4f);
    static float ToSrgb(float c) => c <= 0.0031308f ? c * 12.92f : 1.055f * MathF.Pow(c, 1 / 2.4f) - 0.055f;

    sealed class Renderer(AbstractFileProvider provider)
    {
        readonly ConcurrentDictionary<string, Lazy<MeshData?>> meshes = new(StringComparer.OrdinalIgnoreCase);
        readonly ConcurrentDictionary<string, Lazy<MatInfo>> mats = new(StringComparer.OrdinalIgnoreCase);
        readonly ConcurrentDictionary<string, Lazy<Tex?>> texs = new(StringComparer.OrdinalIgnoreCase);
        public readonly ConcurrentBag<string> Notes = new();

        const string FactoryArray = "FactoryGame/Content/FactoryGame/Buildable/-Shared/Material/Resources/TX2D_FactoryBase_BC.TX2D_FactoryBase_BC";
        /// UV0 cells (u + 3 v) of MM_Factory_Array that carry the primary and the
        /// secondary paint. The material's graph isn't in the cooked files, so this was
        /// matched against the game's own 512 px building icons (default paint): on the
        /// Constructor, cell 0 is the orange panels and lamp post, cell 4 the blue-grey
        /// body (icon pixel 90,85,132 vs FICSIT secondary 95,102,140).
        const int PrimaryCell = 0, SecondaryCell = 4;

        /// Linear default paint: FICSIT primary / secondary.
        public Vector3 Primary = new(-1), Secondary = new(-1);
        /// Mean linear albedo of each TX2D_FactoryBase_BC slice.
        Vector3[] arraySlices = [];
        /// MM_Factory_Array's colour per 3x3 cell.
        Vector3[] arrayCells = [];

        // ---------------------------------------------------------- paint

        public void LoadPaint()
        {
            var gs = provider.LoadPackageObject("FactoryGame/Content/FactoryGame/-Shared/Blueprint/BP_GameState.Default__BP_GameState_C");
            if (!gs.TryGetValue(out FStructFallback[] presets, "mStaticGlobalColorPresets"))
                throw new InvalidDataException("BP_GameState has no mStaticGlobalColorPresets");
            foreach (var p in presets)
            {
                if (!p.TryGetValue(out FText name, "PresetName") || !p.TryGetValue(out FLinearColor c, "Color")) continue;
                var key = name.TextHistory switch
                {
                    FTextHistory.StringTableEntry st => st.Key,
                    FTextHistory.Base b => b.Key,
                    _ => "",
                };
                var v = new Vector3(c.R, c.G, c.B);
                if (key.EndsWith("/FICSITPrimary")) Primary = v;
                if (key.EndsWith("/FICSITSecondary")) Secondary = v;
            }
            if (Primary == new Vector3(-1) || Secondary == new Vector3(-1))
                throw new InvalidDataException("BP_GameState has no FICSITPrimary/FICSITSecondary colour preset");
            Notes.Add($"paint: primary {Primary} secondary {Secondary} (linear, BP_GameState mStaticGlobalColorPresets)");

            // MM_Factory_Array draws each triangle with one slice of this array, picked by
            // which third of UV0 (3 x 3 cells) the triangle lies in. Unpainted cells get
            // their slice's mean colour; the two paint cells get the default paint.
            var arr = provider.LoadPackageObject<UTexture2DArray>(FactoryArray);
            var slices = TextureDecoder.DecodeTextureArray(arr, ETexturePlatform.DesktopMobile)
                ?? throw new InvalidDataException("cannot decode " + FactoryArray);
            arraySlices = slices.Select(t =>
            {
                using var bmp = TextureEncoder.ToSkBitmap(t);
                using var rgba = bmp.Copy(SKColorType.Rgba8888);
                var px = rgba.GetPixelSpan();
                var sum = Vector3.Zero;
                for (var i = 0; i < px.Length; i += 4) sum += new Vector3(ToLinear(px[i] / 255f), ToLinear(px[i + 1] / 255f), ToLinear(px[i + 2] / 255f));
                return sum / (px.Length / 4);
            }).ToArray();
            if (arraySlices.Length != 9) throw new InvalidDataException($"{FactoryArray} has {arraySlices.Length} slices, expected 9");
            arrayCells = arraySlices.ToArray();
            arrayCells[PrimaryCell] = Primary;
            arrayCells[SecondaryCell] = Secondary;
            Notes.Add("factory array slices (linear mean): " + string.Join(" ", arraySlices.Select((v, i) => $"{i}:{v.X:F3},{v.Y:F3},{v.Z:F3}")));
        }

        // ---------------------------------------------------------- loading

        static string PackagePath(string objectPath)
        {
            // "/Game/X/Y.Z" -> "FactoryGame/Content/X/Y.Z"
            if (objectPath.StartsWith("/Game/")) return "FactoryGame/Content/" + objectPath["/Game/".Length..];
            return objectPath.TrimStart('/');
        }

        UObject? LoadClass(Job job)
        {
            string? path = null;
            if (job.FullName is { } full && full.StartsWith("BlueprintGeneratedClass "))
                path = PackagePath(full["BlueprintGeneratedClass ".Length..].Trim());
            if (path is null)
            {
                // Docs without a FullName: find Build_X.uasset among the buildables.
                var stem = job.Class[..^2];
                var hit = provider.Files.Keys.FirstOrDefault(k => k.StartsWith("FactoryGame/Content/FactoryGame/Buildable/", StringComparison.OrdinalIgnoreCase)
                    && k.EndsWith("/" + stem + ".uasset", StringComparison.OrdinalIgnoreCase));
                if (hit is null) return null;
                path = hit[..^".uasset".Length] + "." + job.Class;
            }
            return provider.LoadPackageObject(path);
        }

        MeshData? Mesh(string key, FPackageIndex index) => meshes.GetOrAdd(key, _ => new Lazy<MeshData?>(() =>
        {
            if (!index.TryLoad(out var obj) || obj is not UStaticMesh sm) return null;
            var lod = sm.RenderData?.LODs?.FirstOrDefault(l => !l.SkipLod && l.PositionVertexBuffer is { } && l.IndexBuffer is { } && l.VertexBuffer is { });
            if (lod is null) return null;
            var pos = lod.PositionVertexBuffer!.Verts.Select(v => new Vector3(v.X, v.Y, v.Z)).ToArray();
            var uv = lod.VertexBuffer!.UV.Select(u => u.UV.Length > 0 ? new Vector2(u.UV[0].U, u.UV[0].V) : Vector2.Zero).ToArray();
            var ib = lod.IndexBuffer!;
            var idx = new uint[ib.Length];
            for (var i = 0; i < idx.Length; i++) idx[i] = (uint)ib[i];
            if (idx.Any(i => i >= pos.Length) || uv.Length != pos.Length) throw new InvalidDataException($"{sm.Name}: bad index/vertex buffers");
            return new MeshData
            {
                Name = sm.Name,
                Pos = pos, Uv0 = uv, Idx = idx,
                Sections = lod.Sections.Select(s => (s.MaterialIndex, s.FirstIndex, s.NumTriangles)).ToArray(),
                Materials = (sm.StaticMaterials ?? []).Select(m => (FPackageIndex?)m.MaterialInterface).ToArray(),
            };
        })).Value;

        Tex? Texture(FPackageIndex index) => index.ResolvedObject is { } ro ? texs.GetOrAdd(ro.GetPathName(), _ => new Lazy<Tex?>(() =>
        {
            if (!index.TryLoad(out var obj) || obj is not UTexture2D t) return null;
            var decoded = TextureDecoder.Decode(t, 512, ETexturePlatform.DesktopMobile);
            if (decoded is null) return null;
            using var bmp = TextureEncoder.ToSkBitmap(decoded);
            using var rgba = bmp.Copy(SKColorType.Rgba8888);
            return new Tex { W = rgba.Width, H = rgba.Height, Rgba = rgba.GetPixelSpan().ToArray(), Srgb = t.SRGB };
        })).Value : null;

        MatInfo Material(FPackageIndex? index)
        {
            if (index is null || index.IsNull || index.ResolvedObject is not { } ro) return new MatInfo { Kind = Kind.Other, Root = "(none)" };
            return mats.GetOrAdd(ro.GetPathName(), _ => new Lazy<MatInfo>(() =>
            {
                var info = new MatInfo();
                var textures = new Dictionary<string, FPackageIndex>(StringComparer.OrdinalIgnoreCase);
                var vectors = new Dictionary<string, FLinearColor>(StringComparer.OrdinalIgnoreCase);
                UObject? cur = index.TryLoad(out var o) ? o : null;
                for (var depth = 0; cur is not null && depth < 16; depth++)
                {
                    if (cur is UMaterialInstanceConstant mic)
                    {
                        foreach (var t in mic.TextureParameterValues ?? [])
                            if (t.ParameterInfo?.Name.Text is { } tn && t.ParameterValue is { } tv) textures.TryAdd(tn, tv);
                        foreach (var v in mic.VectorParameterValues ?? [])
                            if (v.ParameterInfo?.Name.Text is { } vn && v.ParameterValue is { } c) vectors.TryAdd(vn, c);
                    }
                    if (cur is UMaterialInstance mi && mi.Parent is { IsNull: false } parent)
                    {
                        cur = parent.TryLoad(out var p) ? p : null;
                        continue;
                    }
                    info.Root = cur.Name;
                    info.Blend = cur.TryGetValue(out FName bm, "BlendMode") ? bm.Text : "BLEND_Opaque";
                    info.Domain = cur.TryGetValue(out FName dm, "MaterialDomain") ? dm.Text : "MD_Surface";
                    break;
                }
                var root = info.Root;
                // Not drawn: decals, see-through and additive surfaces (glass, fog planes, glows), LOD stand-ins.
                if (info.Domain != "MD_Surface" || info.Blend is "BLEND_Translucent" or "BLEND_Additive" or "BLEND_Modulate"
                    || root.Contains("Decal", StringComparison.OrdinalIgnoreCase) || root == "MM_Factory_LOD")
                {
                    info.Kind = Kind.Skip;
                    info.Translucent = info.Domain == "MD_Surface" && info.Blend == "BLEND_Translucent" && !root.Contains("Decal", StringComparison.OrdinalIgnoreCase);
                    return info;
                }
                if (root == "MM_Factory_Array")
                {
                    info.Kind = Kind.Array;
                    return info;
                }
                FPackageIndex? Tx(params string[] names) => names.Select(n => textures.GetValueOrDefault(n)).FirstOrDefault(t => t is { IsNull: false });
                info.Albedo = Tx("Albedo", "BaseColor", "BaseColour", "Base Color", "Diffuse", "BaseColorTexture", "Albedo Texture") is { } a ? Texture(a) : null;
                if (root.StartsWith("MM_FactoryBaked") || info.Albedo is not null)
                {
                    info.Kind = Kind.Baked;
                    info.Masks = Tx("AOMasks") is { } m ? Texture(m) : null;
                    return info;
                }
                info.Kind = Kind.Other;
                foreach (var n in new[] { "BaseColor", "Base Color", "Color", "Albedo Color", "Tint" })
                    if (vectors.TryGetValue(n, out var c)) { info.Flat = new Vector3(c.R, c.G, c.B); break; }
                return info;
            })).Value;
        }

        // ------------------------------------------------------ components

        static FPackageIndex? Cdo(UObject c) => c is UClass uc && uc.ClassDefaultObject is { IsNull: false } i ? i : null;

        static FPackageIndex? Index(IPropertyHolder o, string name) => o.TryGetValue(out FPackageIndex i, name) && !i.IsNull ? i : null;

        static Affine Relative(IEnumerable<UObject> templates)
        {
            FVector? loc = null, scale = null;
            FRotator? rot = null;
            foreach (var t in templates)
            {
                if (loc is null && t.TryGetValue(out FVector l, "RelativeLocation")) loc = l;
                if (rot is null && t.TryGetValue(out FRotator r, "RelativeRotation")) rot = r;
                if (scale is null && t.TryGetValue(out FVector s, "RelativeScale3D")) scale = s;
            }
            return Affine.From((rot ?? new FRotator(0, 0, 0)).Quaternion(), loc ?? new FVector(0, 0, 0), scale ?? new FVector(1, 1, 1));
        }

        static T? First<T>(IEnumerable<UObject> templates, string name) where T : struct
        {
            foreach (var t in templates) if (t.TryGetValue(out T v, name)) return v;
            return null;
        }

        sealed class Node
        {
            public string Var = "";
            public UObject? Original;
            public List<UObject> Overrides = new(); // most derived first
            public IEnumerable<UObject> Templates => Original is null ? Overrides : Overrides.Append(Original);
            public string? Parent;
            public bool ParentNative;
            /// Attached to a socket/bone of its parent (e.g. a skeletal mesh): position unknown here.
            public string? Socket;
        }

        /// Every static mesh the class's Blueprint shows, in actor space.
        List<Part> Parts(UObject cls, Job job)
        {
            // The chain of Blueprint classes, most derived first.
            var chain = new List<UObject>();
            for (UObject? c = cls; c is not null && chain.Count < 16;)
            {
                chain.Add(c);
                c = c is UStruct us && us.SuperStruct is { IsNull: false } sup && sup.TryLoad(out var s) && s is UBlueprintGeneratedClass ? s : null;
            }

            // SCS nodes from the base class down; ICH overrides from the most derived class.
            var nodes = new Dictionary<string, Node>(StringComparer.Ordinal);
            for (var i = chain.Count - 1; i >= 0; i--)
            {
                if (Index(chain[i], "SimpleConstructionScript") is not { } scsIdx || !scsIdx.TryLoad(out var scs)) continue;
                if (!scs.TryGetValue(out FPackageIndex[] all, "AllNodes")) continue;
                var byObj = new Dictionary<string, Node>();
                var loaded = new List<(UObject, Node)>();
                foreach (var ni in all)
                {
                    if (!ni.TryLoad(out var n)) continue;
                    var v = n.TryGetValue(out FName vn, "InternalVariableName") ? vn.Text : n.Name;
                    var node = new Node { Var = v };
                    if (Index(n, "ComponentTemplate") is { } ti && ti.TryLoad(out var tmpl)) node.Original = tmpl;
                    if (n.TryGetValue(out FName pn, "ParentComponentOrVariableName") && !pn.IsNone) node.Parent = pn.Text;
                    node.ParentNative = n.TryGetValue(out bool pnat, "bIsParentComponentNative") && pnat;
                    if (n.TryGetValue(out FName socket, "AttachToName") && !socket.IsNone) node.Socket = socket.Text;
                    nodes[v] = node;
                    loaded.Add((n, node));
                }
                foreach (var (n, node) in loaded)
                {
                    if (!n.TryGetValue(out FPackageIndex[] kids, "ChildNodes")) continue;
                    foreach (var k in kids)
                    {
                        if (!k.TryLoad(out var kn)) continue;
                        var kv = kn.TryGetValue(out FName kvn, "InternalVariableName") ? kvn.Text : kn.Name;
                        if (nodes.TryGetValue(kv, out var child)) { child.Parent = node.Var; child.ParentNative = false; }
                    }
                }
            }
            for (var i = 0; i < chain.Count; i++)
            {
                if (Index(chain[i], "InheritableComponentHandler") is not { } ichIdx || !ichIdx.TryLoad(out var ich)) continue;
                if (!ich.TryGetValue(out FStructFallback[] records, "Records")) continue;
                foreach (var rec in records)
                {
                    if (!rec.TryGetValue(out FStructFallback key, "ComponentKey") || !key.TryGetValue(out FName vn, "SCSVariableName")) continue;
                    if (!nodes.TryGetValue(vn.Text, out var node)) continue;
                    if (Index(rec, "ComponentTemplate") is { } ti && ti.TryLoad(out var tmpl)) node.Overrides.Add(tmpl);
                }
            }

            // Actor-space transform of each node through its parents.
            var world = new Dictionary<string, Affine>(StringComparer.Ordinal);
            Affine World(Node n, int depth)
            {
                if (world.TryGetValue(n.Var, out var w)) return w;
                var parent = Affine.Identity;
                if (depth < 32 && n.Parent is { } p && !n.ParentNative && nodes.TryGetValue(p, out var pn)) parent = World(pn, depth + 1);
                w = parent.Then(Relative(n.Templates));
                world[n.Var] = w;
                return w;
            }

            string? SocketOf(Node n)
            {
                for (var (cur, d) = (n, 0); cur is not null && d < 32; d++)
                {
                    if (cur.Socket is { } so) return so;
                    cur = cur.Parent is { } p && !cur.ParentNative && nodes.TryGetValue(p, out var pn) ? pn : null!;
                }
                return null;
            }

            var parts = new List<Part>();
            foreach (var n in nodes.Values)
            {
                if (!n.Templates.Any()) continue;
                var mesh = n.Templates.Select(t => Index(t, "StaticMesh")).FirstOrDefault(i => i is not null);
                if (mesh is null) continue;
                if (First<bool>(n.Templates, "bVisible") == false || First<bool>(n.Templates, "bHiddenInGame") == true) continue;
                if (SocketOf(n) is { } sock)
                {
                    Notes.Add($"{job.Class}: {n.Var} skipped: attached to socket/bone {sock} of a parent");
                    continue;
                }
                if (n.ParentNative && n.Parent is { } np && np != "RootComponent")
                    Notes.Add($"{job.Class}: {n.Var} is attached to native {np} (treated as the root)");
                var over = n.Templates.Select(t => t.TryGetValue(out FPackageIndex[] o, "OverrideMaterials") ? o : null).FirstOrDefault(o => o is not null);
                parts.Add(new Part(mesh.ResolvedObject!.GetPathName(), World(n, 0), over?.Select(x => (FPackageIndex?)x).ToArray() ?? [], $"scs:{n.Var}:{n.Templates.First().ExportType}"));
                meshIndex[parts[^1].Mesh] = mesh;
            }

            // Abstract instances (lightweight buildables), from the nearest CDO that sets them.
            foreach (var c in chain)
            {
                if (Cdo(c) is not { } cdoIdx || !cdoIdx.TryLoad(out var cdo)) continue;
                if (Index(cdo, "mInstanceDataCDO") is not { } idIdx) continue;
                if (idIdx.TryLoad(out var data) && data.TryGetValue(out FStructFallback[] instances, "Instances"))
                {
                    foreach (var inst in instances)
                    {
                        if (Index(inst, "StaticMesh") is not { } mesh) continue;
                        var xf = inst.TryGetValue(out FStructFallback t, "RelativeTransform") ? TransformOf(t) : Affine.Identity;
                        var over = inst.TryGetValue(out FPackageIndex[] o, "OverridenMaterials") ? o.Select(x => (FPackageIndex?)x).ToArray() : [];
                        parts.Add(new Part(mesh.ResolvedObject!.GetPathName(), xf, over, "instance"));
                        meshIndex[parts[^1].Mesh] = mesh;
                    }
                }
                break;
            }

            // Lifts: their CDO names the meshes they stack.
            if (job.Native == "FGBuildableConveyorLift")
            {
                foreach (var c in chain)
                {
                    if (Cdo(c) is not { } cdoIdx || !cdoIdx.TryLoad(out var cdo)) continue;
                    // The meshes live on the CDO or on its sparse data object.
                    var holders = new List<UObject> { cdo };
                    if (Index(cdo, "mConveyorLiftSparesDataCDO") is { } sd && sd.TryLoad(out var sdo)) holders.Add(sdo);
                    foreach (var name in new[] { "mBottomMesh", "mTopMesh" })
                        if (holders.Select(h => Index(h, name)).FirstOrDefault(i => i is not null) is { } mesh)
                        {
                            parts.Add(new Part(mesh.ResolvedObject!.GetPathName(), Affine.Identity, [], "cdo:" + name));
                            meshIndex[parts[^1].Mesh] = mesh;
                        }
                    break;
                }
            }
            return parts;
        }

        readonly ConcurrentDictionary<string, FPackageIndex> meshIndex = new(StringComparer.OrdinalIgnoreCase);

        static Affine TransformOf(FStructFallback t)
        {
            var rot = t.TryGetValue(out FQuat q, "Rotation") ? q : new FQuat(0, 0, 0, 1);
            var pos = t.TryGetValue(out FVector p, "Translation") ? p : new FVector(0, 0, 0);
            var scale = t.TryGetValue(out FVector s, "Scale3D") ? s : new FVector(1, 1, 1);
            return Affine.From(rot, pos, scale);
        }

        // ------------------------------------------------------- rendering

        sealed class Scene
        {
            public readonly List<Vector3> P = new();   // 3 per triangle
            public readonly List<Vector2> Uv = new();  // 3 per triangle
            public readonly List<MatInfo> Mat = new(); // 1 per triangle
        }

        static readonly MatInfo TranslucentFlat = new() { Kind = Kind.Other, Root = "(translucent)" };

        /// <param name="keepTranslucent">Draw see-through surfaces as flat grey (belt strips:
        /// the Mk.6 belt's middle is a translucent glowing strip with nothing under it).</param>
        void Add(Scene scene, Part part, List<string> used, bool keepTranslucent = false)
        {
            if (!meshIndex.TryGetValue(part.Mesh, out var idx) || Mesh(part.Mesh, idx) is not { } mesh)
            {
                Notes.Add($"mesh {part.Mesh}: not loadable");
                return;
            }
            used.Add($"{mesh.Name}[{part.Source}]");
            foreach (var (matIdx, first, tris) in mesh.Sections)
            {
                FPackageIndex? m = matIdx < part.Overrides.Count && part.Overrides[matIdx] is { IsNull: false } o ? o
                    : matIdx < mesh.Materials.Length ? mesh.Materials[matIdx] : null;
                var mat = Material(m);
                if (mat.Kind == Kind.Skip && keepTranslucent && mat.Translucent) mat = TranslucentFlat;
                if (mat.Kind == Kind.Skip) continue;
                for (var t = 0; t < tris; t++)
                {
                    for (var k = 0; k < 3; k++)
                    {
                        var vi = mesh.Idx[first + t * 3 + k];
                        scene.P.Add(part.Xf.Apply(mesh.Pos[vi]));
                        scene.Uv.Add(mesh.Uv0[vi]);
                    }
                    scene.Mat.Add(mat);
                }
            }
        }

        public (byte[] png, Vector2 min, Vector2 max) RenderBuilding(Job job)
        {
            var cls = LoadClass(job) ?? throw new InvalidDataException("Blueprint class not found");
            var parts = Parts(cls, job);
            var scene = new Scene();
            var used = new List<string>();
            foreach (var p in parts) Add(scene, p, used);
            if (scene.Mat.Count == 0) throw new InvalidDataException("no visible static mesh");
            Notes.Add($"{job.Class}: {string.Join(", ", used)}");

            var lo = new Vector2(float.MaxValue); var hi = new Vector2(float.MinValue);
            for (var t = 0; t < scene.Mat.Count; t++)
            {
                Vector3 a = scene.P[t * 3], b = scene.P[t * 3 + 1], c = scene.P[t * 3 + 2];
                if (Vector3.Cross(b - a, c - a).Length() < 1e-3f) continue;
                foreach (var p in new[] { a, b, c }) { lo = Vector2.Min(lo, new Vector2(p.X, p.Y)); hi = Vector2.Max(hi, new Vector2(p.X, p.Y)); }
            }
            // Snap to whole pixels on the cm grid, one pixel of margin.
            var min = new Vector2(MathF.Floor(lo.X / CmPerPx) - 1, MathF.Floor(lo.Y / CmPerPx) - 1) * CmPerPx;
            var max = new Vector2(MathF.Ceiling(hi.X / CmPerPx) + 1, MathF.Ceiling(hi.Y / CmPerPx) + 1) * CmPerPx;
            var w = (int)MathF.Round((max.X - min.X) / CmPerPx);
            var h = (int)MathF.Round((max.Y - min.Y) / CmPerPx);
            if (w <= 0 || h <= 0 || w > MaxSidePx || h > MaxSidePx) throw new InvalidDataException($"unexpected size {w}x{h}");
            var rgba = Rasterize(scene, w, h, p => new Vector2((p.X - min.X) / CmPerPx, (p.Y - min.Y) / CmPerPx));
            // Some meshes carry stray far-away slivers that touch a sample or two: crop to
            // pixels at least a quarter covered.
            int x0 = w, y0 = h, x1 = -1, y1 = -1;
            for (var y = 0; y < h; y++)
                for (var x = 0; x < w; x++)
                    if (rgba[(y * w + x) * 4 + 3] >= 64) { x0 = Math.Min(x0, x); x1 = Math.Max(x1, x); y0 = Math.Min(y0, y); y1 = Math.Max(y1, y); }
            if (x1 < 0) throw new InvalidDataException("nothing drawn");
            x0 = Math.Max(0, x0 - 1); y0 = Math.Max(0, y0 - 1); x1 = Math.Min(w - 1, x1 + 1); y1 = Math.Min(h - 1, y1 + 1);
            int cw = x1 - x0 + 1, ch = y1 - y0 + 1;
            var crop = new byte[cw * ch * 4];
            for (var y = 0; y < ch; y++) Array.Copy(rgba, ((y + y0) * w + x0) * 4, crop, y * cw * 4, cw * 4);
            var cmin = min + new Vector2(x0, y0) * CmPerPx;
            return (Png(crop, cw, ch), cmin, cmin + new Vector2(cw, ch) * CmPerPx);
        }

        public (byte[] png, float widthCm, float lengthCm) RenderStrip(Job job)
        {
            var cls = LoadClass(job) ?? throw new InvalidDataException("Blueprint class not found");
            FPackageIndex? meshIdx = null;
            FPackageIndex? matIdx = null;
            for (UObject? c = cls; c is not null && meshIdx is null;)
            {
                if (Cdo(c) is { } cdoIdx && cdoIdx.TryLoad(out var cdo))
                {
                    meshIdx = Index(cdo, "mMesh");
                    matIdx ??= Index(cdo, "mSplineMeshMaterial");
                }
                c = c is UStruct us && us.SuperStruct is { IsNull: false } sup && sup.TryLoad(out var s) && s is UBlueprintGeneratedClass ? s : null;
            }
            if (meshIdx is null) throw new InvalidDataException("no mMesh on the CDO");
            var key = meshIdx.ResolvedObject!.GetPathName();
            meshIndex[key] = meshIdx;
            var mesh = Mesh(key, meshIdx) ?? throw new InvalidDataException("mesh not loadable");
            var length = job.MeshLength > 0 ? job.MeshLength : throw new InvalidDataException("Docs give no mMeshLength");

            // Spline meshes stretch the mesh's X extent over each segment.
            float x0 = mesh.Pos.Min(p => p.X), x1 = mesh.Pos.Max(p => p.X);
            var halfW = mesh.Pos.Max(p => MathF.Abs(p.Y));
            if (!(x1 - x0 > 1)) throw new InvalidDataException("flat strip mesh");
            var stretch = length / (x1 - x0);
            Notes.Add($"{job.Class}: {mesh.Name} X {x0:F1}..{x1:F1} (stretched x{stretch:F3} to mMeshLength {length}), |Y| <= {halfW:F1}");
            var xf = new Affine(new Vector3(stretch, 0, 0), Vector3.UnitY, Vector3.UnitZ, new Vector3(-x0 * stretch, 0, 0));
            var over = matIdx is null ? Array.Empty<FPackageIndex?>() : mesh.Materials.Select(_ => (FPackageIndex?)matIdx).ToArray();
            var scene = new Scene();
            Add(scene, new Part(key, xf, over, "cdo:mMesh"), new List<string>(), keepTranslucent: true);
            var half = MathF.Ceiling(halfW / CmPerPx) * CmPerPx;
            var w = (int)MathF.Round(length / CmPerPx);
            var h = (int)MathF.Round(2 * half / CmPerPx);
            var png = Png(Rasterize(scene, w, h, p => new Vector2(p.X / CmPerPx, (p.Y + half) / CmPerPx)), w, h);
            return (png, 2 * half, length);
        }

        static readonly Vector3 Light = Vector3.Normalize(new Vector3(-0.35f, -0.5f, 1f));
        const float Ambient = 0.42f;

        /// Straight-alpha RGBA, w x h.
        byte[] Rasterize(Scene scene, int w, int h, Func<Vector3, Vector2> toPx)
        {
            var s = Supersample;
            while (s > 1 && (long)w * h * s * s > MaxSamples) s--;
            int sw = w * s, sh = h * s;
            var depth = new float[sw * sh];
            Array.Fill(depth, float.NegativeInfinity);
            var tri = new int[sw * sh];
            Array.Fill(tri, -1);
            var bary = new Vector2[sw * sh];

            var n = scene.Mat.Count;
            for (var t = 0; t < n; t++)
            {
                var p0 = scene.P[t * 3]; var p1 = scene.P[t * 3 + 1]; var p2 = scene.P[t * 3 + 2];
                var a = toPx(p0) * s; var b = toPx(p1) * s; var c = toPx(p2) * s;
                var area = (b.X - a.X) * (c.Y - a.Y) - (b.Y - a.Y) * (c.X - a.X);
                if (MathF.Abs(area) < 1e-9f) continue;
                int xa = Math.Max(0, (int)MathF.Floor(MathF.Min(a.X, MathF.Min(b.X, c.X)))), xb = Math.Min(sw - 1, (int)MathF.Ceiling(MathF.Max(a.X, MathF.Max(b.X, c.X))));
                int ya = Math.Max(0, (int)MathF.Floor(MathF.Min(a.Y, MathF.Min(b.Y, c.Y)))), yb = Math.Min(sh - 1, (int)MathF.Ceiling(MathF.Max(a.Y, MathF.Max(b.Y, c.Y))));
                var inv = 1f / area;
                for (var y = ya; y <= yb; y++)
                {
                    var py = y + 0.5f;
                    for (var x = xa; x <= xb; x++)
                    {
                        var px = x + 0.5f;
                        var w0 = ((b.X - px) * (c.Y - py) - (b.Y - py) * (c.X - px)) * inv;
                        var w1 = ((c.X - px) * (a.Y - py) - (c.Y - py) * (a.X - px)) * inv;
                        var w2 = 1 - w0 - w1;
                        if (w0 < 0 || w1 < 0 || w2 < 0) continue;
                        var z = w0 * p0.Z + w1 * p1.Z + w2 * p2.Z;
                        var i = y * sw + x;
                        if (z <= depth[i]) continue;
                        depth[i] = z; tri[i] = t; bary[i] = new Vector2(w0, w1);
                    }
                }
            }

            // Shade each sample once, then average s x s samples per pixel.
            var normals = new Vector3[n];
            for (var t = 0; t < n; t++)
            {
                var nn = Vector3.Cross(scene.P[t * 3 + 1] - scene.P[t * 3], scene.P[t * 3 + 2] - scene.P[t * 3]);
                var len = nn.Length();
                nn = len > 0 ? nn / len : Vector3.UnitZ;
                normals[t] = nn.Z < 0 ? -nn : nn;
            }
            var outPx = new byte[w * h * 4];
            for (var y = 0; y < h; y++)
                for (var x = 0; x < w; x++)
                {
                    var sum = Vector3.Zero; var cnt = 0;
                    for (var sy = 0; sy < s; sy++)
                        for (var sx = 0; sx < s; sx++)
                        {
                            var i = (y * s + sy) * sw + x * s + sx;
                            var t = tri[i];
                            if (t < 0) continue;
                            var bc = bary[i];
                            var uv = scene.Uv[t * 3] * bc.X + scene.Uv[t * 3 + 1] * bc.Y + scene.Uv[t * 3 + 2] * (1 - bc.X - bc.Y);
                            var albedo = Albedo(scene.Mat[t], uv, scene.Uv[t * 3], scene.Uv[t * 3 + 1], scene.Uv[t * 3 + 2]);
                            var shade = Ambient + (1 - Ambient) * MathF.Max(0, Vector3.Dot(normals[t], Light));
                            sum += albedo * shade; cnt++;
                        }
                    if (cnt == 0) continue;
                    var col = sum / cnt;
                    var o = (y * w + x) * 4;
                    outPx[o] = (byte)Math.Clamp(MathF.Round(ToSrgb(Math.Clamp(col.X, 0, 1)) * 255), 0, 255);
                    outPx[o + 1] = (byte)Math.Clamp(MathF.Round(ToSrgb(Math.Clamp(col.Y, 0, 1)) * 255), 0, 255);
                    outPx[o + 2] = (byte)Math.Clamp(MathF.Round(ToSrgb(Math.Clamp(col.Z, 0, 1)) * 255), 0, 255);
                    outPx[o + 3] = (byte)Math.Clamp(MathF.Round(255f * cnt / (s * s)), 0, 255);
                }
            return outPx;
        }

        static byte[] Png(byte[] rgba, int w, int h)
        {
            using var bmp = new SKBitmap(new SKImageInfo(w, h, SKColorType.Rgba8888, SKAlphaType.Unpremul));
            System.Runtime.InteropServices.Marshal.Copy(rgba, 0, bmp.GetPixels(), rgba.Length);
            using var img = SKImage.FromBitmap(bmp);
            using var data = img.Encode(SKEncodedImageFormat.Png, 100);
            return data.ToArray();
        }

        /// Linear albedo of a sample.
        Vector3 Albedo(MatInfo m, Vector2 uv, Vector2 uvA, Vector2 uvB, Vector2 uvC)
        {
            switch (m.Kind)
            {
                case Kind.Array:
                {
                    // MM_Factory_Array: UV0 picks one of 3 x 3 cells per triangle.
                    var centre = (uvA + uvB + uvC) / 3;
                    int cx = Math.Clamp((int)MathF.Floor(centre.X * 3), 0, 2), cy = Math.Clamp((int)MathF.Floor(centre.Y * 3), 0, 2);
                    return ArrayCell(cx + 3 * cy);
                }
                case Kind.Baked:
                {
                    var a = m.Albedo is { } t ? Lin(t.Sample(uv)) : new Vector3(0.5f);
                    if (m.Masks is { } mk)
                    {
                        var mask = mk.Sample(uv);
                        a *= mask.X; // R: ambient occlusion
                        // G: primary paint (on the splitters it is exactly their orange
                        // corners in the game's icon). B is left alone: no mask seen here
                        // could be checked against an icon.
                        a = Vector3.Lerp(a, a * Primary, mask.Y);
                    }
                    return a;
                }
                default:
                    return m.Albedo is { } tex ? Lin(tex.Sample(uv)) : m.Flat;
            }
        }

        static Vector3 Lin(Vector4 c) => new(ToLinear(c.X), ToLinear(c.Y), ToLinear(c.Z));

        Vector3 ArrayCell(int i) => i < arrayCells.Length ? arrayCells[i] : new Vector3(0.214f);
    }
}
