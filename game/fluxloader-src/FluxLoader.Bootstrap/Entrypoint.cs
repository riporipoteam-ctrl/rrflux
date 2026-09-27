// FluxLoader.Bootstrap — the managed entrypoint Doorstop 4.5.0 calls.
//
// Doorstop hosts a second (CoreCLR 6.0.7) runtime inside the game process,
// loads THIS dll (doorstop_config.ini -> target_assembly), and invokes
//     Doorstop.Entrypoint.Start()
// The namespace/class/method names are a hard contract — do not rename.
//
// Boot sequence:
//   1. Install an AssemblyLoadContext resolving handler so dependencies
//      (Il2CppInterop.Runtime, 0Harmony, plugin deps) resolve from
//      FluxLoader\core\ without probing the game dir.
//   2. Spawn a background watcher thread that polls il2cpp_domain_get()
//      (P/Invoke into GameAssembly.dll) until the IL2CPP domain exists.
//      This runs BEFORE il2cpp_init finishes; no native hooks required.
//   3. Once the domain is up (+ a short settle delay so the game's own
//      assemblies are registered), start Il2CppInteropRuntime with the
//      pre-generated interop assemblies in FluxLoader\interop\.
//   4. Scan FluxLoader\plugins\*.dll for IFluxPlugin implementations and
//      call Load() on each.
//
// Golden rule: the loader must NEVER crash the game. Every boundary that
// touches loader code is wrapped in try/catch; failures go to the log.

using System;
using System.IO;
using System.Linq;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Runtime.Loader;
using System.Threading;
using FluxLoader;
using Il2CppInterop.Runtime.Startup;

namespace Doorstop
{
    /// <summary>
    /// Doorstop entrypoint. Exact name/signature required by Doorstop 4.x.
    /// </summary>
    public static class Entrypoint
    {
        // ------------------------------------------------------------------
        // Paths
        // ------------------------------------------------------------------

        /// <summary>
        /// Directory of this bootstrap DLL == FluxLoader\core\.
        /// Doorstop sets DOORSTOP_INVOKE_DLL_PATH to the target assembly path;
        /// fall back to our own location in case it is missing.
        /// </summary>
        private static string CoreDir
        {
            get
            {
                var fromEnv = Environment.GetEnvironmentVariable("DOORSTOP_INVOKE_DLL_PATH");
                var path = !string.IsNullOrEmpty(fromEnv)
                    ? Path.GetDirectoryName(fromEnv)
                    : Path.GetDirectoryName(typeof(Entrypoint).Assembly.Location);
                return path ?? AppContext.BaseDirectory;
            }
        }

        /// <summary>FluxLoader\ (parent of core\). Plugins, log and interop live here.</summary>
        private static string LoaderDir => Path.GetFullPath(Path.Combine(CoreDir, ".."));

        // ------------------------------------------------------------------
        // Entry
        // ------------------------------------------------------------------

        /// <summary>
        /// Called by Doorstop before Unity initializes IL2CPP. Blocks until
        /// plugins are loaded (or timeout) — the game must not boot past
        /// IL2CPP init without our Harmony patches (especially the HTTP
        /// redirect) applied, or its "Connecting to server" requests escape
        /// to the dead official backend and hang forever.
        /// </summary>
        public static void Start()
        {
            try
            {
                AssemblyLoadContext.Default.Resolving += OnResolving;

                var version = typeof(Entrypoint).Assembly
                    .GetCustomAttribute<AssemblyInformationalVersionAttribute>()?.InformationalVersion
                    ?? "unknown";

                Log($"FluxLoader bootstrap v{version} starting (core: {CoreDir})");

                var watcher = new Thread(WatchForIl2Cpp)
                {
                    IsBackground = true,
                    Name = "FluxLoader IL2CPP watcher",
                };
                watcher.Start();

                // Block Doorstop until plugins are loaded. The watcher signals
                // via _pluginsReady. Timeout after 3 minutes — if the watcher
                // died, let the game boot unpatched rather than hang forever.
                if (!_pluginsReady.Wait(TimeSpan.FromMinutes(3)))
                {
                    Log("WARNING: timed out waiting for plugins; game will boot without patches.");
                }
                else
                {
                    Log("Plugins ready; releasing game boot.");
                }
            }
            catch (Exception ex)
            {
                // Last resort: if even the startup path throws, log it and die
                // quietly. Never let an exception escape into Doorstop.
                try { Log("FATAL during Start(): " + ex); } catch { /* truly nothing left */ }
            }
        }

        /// <summary>
        /// Signaled by the watcher thread once LoadPlugins() completes
        /// (successfully or not). Start() blocks on this.
        /// </summary>
        private static readonly ManualResetEventSlim _pluginsReady = new(false);

        // ------------------------------------------------------------------
        // Dependency resolution
        // ------------------------------------------------------------------

        /// <summary>
        /// Resolve loader dependencies from FluxLoader\core\.
        /// LoadFromAssemblyPath does not recurse into Resolving, so no
        /// infinite loop is possible. Returns null to let the runtime keep
        /// probing if the file is not ours.
        /// </summary>
        private static Assembly? OnResolving(AssemblyLoadContext context, AssemblyName name)
        {
            try
            {
                if (string.IsNullOrEmpty(name.Name))
                    return null;

                // Assembly file name == simple name + ".dll". Covers 0Harmony.dll
                // (HarmonyX's file name) and Il2CppInterop.*.dll.
                var candidate = Path.Combine(CoreDir, name.Name + ".dll");
                if (File.Exists(candidate))
                    return context.LoadFromAssemblyPath(candidate);

                return null;
            }
            catch (Exception ex)
            {
                Log($"Resolver failed for {name.Name}: {ex.Message}");
                return null;
            }
        }

        // ------------------------------------------------------------------
        // IL2CPP domain watcher
        // ------------------------------------------------------------------

        // il2cpp_domain_get is exported by GameAssembly.dll on Windows IL2CPP
        // builds (Unity 2021). If a future build only exports it from
        // UnityPlayer.dll, add a fallback DllImport there (dumpbin /exports).
        [DllImport("GameAssembly.dll", CallingConvention = CallingConvention.Cdecl)]
        private static extern IntPtr il2cpp_domain_get();

        private const int PollIntervalMs = 100;
        private const int DomainTimeoutMs = 120_000; // give up after 2 minutes
        private const int SettleDelayMs = 2_000;     // let game assemblies register

        private static void WatchForIl2Cpp()
        {
            try
            {
                Log("Waiting for IL2CPP domain...");
                var waited = 0;
                while (true)
                {
                    IntPtr domain;
                    try
                    {
                        domain = il2cpp_domain_get();
                    }
                    catch (Exception ex)
                    {
                        // GameAssembly.dll not loaded yet or export missing.
                        Log($"il2cpp_domain_get unavailable: {ex.GetType().Name}: {ex.Message}");
                        return;
                    }

                    if (domain != IntPtr.Zero)
                        break;

                    if (waited >= DomainTimeoutMs)
                    {
                        Log("Timed out waiting for IL2CPP domain; plugins will not load.");
                        return;
                    }

                    Thread.Sleep(PollIntervalMs);
                    waited += PollIntervalMs;
                }

                Log("IL2CPP domain detected; settling before starting interop runtime...");
                Thread.Sleep(SettleDelayMs);

                StartInteropRuntime();
                LoadPlugins();
            }
            catch (Exception ex)
            {
                // The watcher thread must never take the process down with it.
                try { Log("FATAL in watcher thread: " + ex); } catch { }
            }
            finally
            {
                // Always release Start(), even on failure — the game must boot.
                try { _pluginsReady.Set(); } catch { }
            }
        }

        // ------------------------------------------------------------------
        // Interop runtime
        // ------------------------------------------------------------------

        /// <summary>
        /// Loads every pre-generated interop assembly into the default
        /// load context, then starts the Il2CppInterop runtime.
        ///
        /// NOTE (Il2CppInterop 1.5.3): RuntimeConfiguration has NO
        /// InteropAssemblyPath property (that arrived in 1.6+). The 1.5.3
        /// runtime resolves interop types from assemblies that are already
        /// loaded in the ALC — exactly what BepInEx 6.0.0-pre.2 does with
        /// BepInEx\interop\*.dll — so we eagerly load them ourselves.
        /// </summary>
        private static void StartInteropRuntime()
        {
            try
            {
                var interopPath = Path.Combine(LoaderDir, "interop");
                if (!Directory.Exists(interopPath))
                {
                    Log($"Interop directory missing: {interopPath}; skipping interop start.");
                    return;
                }

                var interopDlls = Directory.GetFiles(interopPath, "*.dll");
                Log($"Loading {interopDlls.Length} interop assemblies from {interopPath}...");
                var loaded = 0;
                foreach (var dll in interopDlls.OrderBy(Path.GetFileName))
                {
                    try
                    {
                        AssemblyLoadContext.Default.LoadFromAssemblyPath(dll);
                        loaded++;
                    }
                    catch (Exception ex)
                    {
                        // One corrupt interop DLL is logged, not fatal; the
                        // runtime will fail loudly later if it was essential.
                        Log($"Interop load warning for {Path.GetFileName(dll)}: {ex.Message}");
                    }
                }
                Log($"Loaded {loaded}/{interopDlls.Length} interop assemblies.");

                Log("Starting Il2CppInteropRuntime...");
                // UnityVersion/DetourProvider left at defaults; the runtime
                // auto-detects from the live domain. If a future Unity build
                // mis-detects, set RuntimeConfiguration.UnityVersion explicitly.
                Il2CppInteropRuntime.Create(new RuntimeConfiguration()).Start();
                Log("Il2CppInteropRuntime started.");
            }
            catch (Exception ex)
            {
                Log("Failed to start interop runtime: " + ex);
            }
        }

        // ------------------------------------------------------------------
        // Plugin loading
        // ------------------------------------------------------------------

        private static void LoadPlugins()
        {
            try
            {
                var pluginsDir = Path.Combine(LoaderDir, "plugins");
                if (!Directory.Exists(pluginsDir))
                {
                    Log($"Plugins directory missing: {pluginsDir}; nothing to load.");
                    return;
                }

                var dlls = Directory.GetFiles(pluginsDir, "*.dll");
                if (dlls.Length == 0)
                {
                    Log("No plugin DLLs found.");
                    return;
                }

                foreach (var dll in dlls)
                    LoadOnePlugin(dll);
            }
            catch (Exception ex)
            {
                Log("Plugin scan failed: " + ex);
            }
        }

        private static void LoadOnePlugin(string dll)
        {
            var fileName = Path.GetFileName(dll);
            try
            {
                var asm = AssemblyLoadContext.Default.LoadFromAssemblyPath(dll);

                var pluginType = GetLoadableTypes(asm).FirstOrDefault(t =>
                    typeof(IFluxPlugin).IsAssignableFrom(t) &&
                    t is { IsAbstract: false, IsInterface: false } &&
                    t.GetConstructor(Type.EmptyTypes) != null);

                if (pluginType == null)
                {
                    Log($"Skipped {fileName}: no public IFluxPlugin implementation with a parameterless constructor.");
                    return;
                }

                Log($"Loading plugin {fileName} ({pluginType.FullName})...");
                var plugin = (IFluxPlugin)Activator.CreateInstance(pluginType)!;
                plugin.Load();
                Log($"Plugin loaded: {fileName}");
            }
            catch (Exception ex)
            {
                // One bad plugin must not block the others or the game.
                Log($"Failed to load plugin {fileName}: {ex}");
            }
        }

        /// <summary>
        /// GetTypes() throws ReflectionTypeLoadException if ANY type fails to
        /// resolve; return the ones that loaded instead of failing the scan.
        /// </summary>
        private static Type[] GetLoadableTypes(Assembly asm)
        {
            try
            {
                return asm.GetTypes();
            }
            catch (ReflectionTypeLoadException ex)
            {
                return ex.Types.Where(t => t != null).ToArray()!;
            }
        }

        // ------------------------------------------------------------------
        // Logging
        // ------------------------------------------------------------------

        private static readonly object LogLock = new();
        private const long MaxLogBytes = 5L * 1024 * 1024; // rotate at 5 MiB

        /// <summary>
        /// Thread-safe file logging to FluxLoader\fluxloader.log. Never throws.
        /// </summary>
        public static void Log(string message)
        {
            try
            {
                lock (LogLock)
                {
                    var logPath = Path.Combine(LoaderDir, "fluxloader.log");
                    var info = new FileInfo(logPath);
                    if (info.Exists && info.Length > MaxLogBytes)
                    {
                        var oldPath = logPath + ".old";
                        if (File.Exists(oldPath)) File.Delete(oldPath);
                        File.Move(logPath, oldPath);
                    }

                    File.AppendAllText(logPath,
                        $"[{DateTime.Now:yyyy-MM-dd HH:mm:ss}] {message}{Environment.NewLine}");
                }
            }
            catch
            {
                // Logging must never throw — especially not on the game thread.
            }
        }
    }
}
