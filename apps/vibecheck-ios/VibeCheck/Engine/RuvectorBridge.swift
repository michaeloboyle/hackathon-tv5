//
// RuvectorBridge.swift
// VibeCheck
//
// Swift wrapper around Ruvector WASM for on-device learning
// Uses WasmKit (https://github.com/swiftwasm/WasmKit) for pure-Swift WASM execution
//

import Foundation
import WasmKit

/// Bridge between VibeCheck and Ruvector WASM recommendation engine
///
/// Provides privacy-preserving on-device learning with high-performance WASM execution.
/// All learning happens locally - zero network requests.
@available(iOS 15.0, *)
class RuvectorBridge {

    // MARK: - Types

    enum RuvectorError: Error, LocalizedError {
        case wasmNotLoaded
        case invalidPath
        case loadFailed(String)
        case functionNotFound(String)
        case instantiationFailed(String)

        var errorDescription: String? {
            switch self {
            case .wasmNotLoaded: return "WASM module not loaded"
            case .invalidPath: return "Invalid WASM file path"
            case .loadFailed(let msg): return "Failed to load WASM: \(msg)"
            case .functionNotFound(let name): return "Function '\(name)' not found in WASM exports"
            case .instantiationFailed(let msg): return "WASM instantiation failed: \(msg)"
            }
        }
    }

    // MARK: - Properties

    private var engine: Engine?
    private var store: Store?
    private var wasmModule: Module?
    private var wasmInstance: Instance?

    /// Whether the WASM module is loaded and ready
    private(set) var isReady: Bool = false

    /// Time taken to load the WASM module (for benchmarking)
    private(set) var loadTimeMs: Double = 0

    /// List of exported function names (for debugging)
    private(set) var exportedFunctions: [String] = []

    // MARK: - Initialization

    init() {}

    // MARK: - Lifecycle

    /// Load the WASM module from a file path
    ///
    /// - Parameter wasmPath: Path to the .wasm file
    /// - Throws: RuvectorError if loading fails
    func load(wasmPath: String) async throws {
        guard FileManager.default.fileExists(atPath: wasmPath) else {
            throw RuvectorError.invalidPath
        }

        let startTime = CFAbsoluteTimeGetCurrent()

        do {
            // Load WASM bytes
            let wasmData = try Data(contentsOf: URL(fileURLWithPath: wasmPath))
            let wasmBytes = Array(wasmData)

            // Create WasmKit engine and store
            self.engine = Engine()
            self.store = Store(engine: engine!)

            // Parse WASM module using WasmKit's parseWasm function
            self.wasmModule = try parseWasm(bytes: wasmBytes)

            // Create empty imports (WASI functions would go here if needed)
            let imports = Imports()

            // Instantiate module
            self.wasmInstance = try wasmModule!.instantiate(store: store!, imports: imports)

            // Record exported functions for debugging
            self.exportedFunctions = wasmInstance!.exports.map { $0.0 }

            // Calculate load time
            self.loadTimeMs = (CFAbsoluteTimeGetCurrent() - startTime) * 1000

            self.isReady = true
            print("✅ RuvectorBridge: WASM loaded in \(String(format: "%.1f", loadTimeMs))ms")
            print("   Exports: \(exportedFunctions.joined(separator: ", "))")

        } catch let error as RuvectorError {
            throw error
        } catch {
            throw RuvectorError.loadFailed(error.localizedDescription)
        }
    }

    /// Load the WASM module from bundle
    func loadFromBundle() async throws {
        guard let path = Bundle.main.path(forResource: "ruvector", ofType: "wasm") else {
            throw RuvectorError.invalidPath
        }
        try await load(wasmPath: path)
    }

    // MARK: - Export Access

    /// Check if an exported function exists
    private func hasExportedFunction(name: String) -> Bool {
        guard let instance = wasmInstance else { return false }
        if case .function(_) = instance.export(name) {
            return true
        }
        return false
    }

    /// Get an exported function by name
    private func getExportedFunction(name: String) -> Function? {
        guard let instance = wasmInstance else { return nil }
        if case .function(let function) = instance.export(name) {
            return function
        }
        return nil
    }

    // MARK: - Benchmarking

    /// Benchmark WASM module loading
    static func benchmarkLoad(wasmPath: String) async -> (success: Bool, timeMs: Double, error: String?) {
        let bridge = RuvectorBridge()
        do {
            try await bridge.load(wasmPath: wasmPath)
            return (true, bridge.loadTimeMs, nil)
        } catch {
            return (false, 0, error.localizedDescription)
        }
    }

    /// Benchmark a simple WASM operation
    func benchmarkSimpleOp(iterations: Int = 1000) throws -> Double {
        guard isReady else {
            throw RuvectorError.wasmNotLoaded
        }

        // Look for a simple benchmark function
        if let benchFunc = getExportedFunction(name: "benchmark") {
            let start = CFAbsoluteTimeGetCurrent()

            for _ in 0..<iterations {
                _ = try benchFunc.invoke([])
            }

            let totalTime = CFAbsoluteTimeGetCurrent() - start
            return (totalTime * 1000) / Double(iterations)
        }

        // Fallback: benchmark add function if available
        if let addFunc = getExportedFunction(name: "add") {
            let start = CFAbsoluteTimeGetCurrent()

            for i in 0..<iterations {
                _ = try addFunc.invoke([.i32(UInt32(i)), .i32(UInt32(i + 1))])
            }

            let totalTime = CFAbsoluteTimeGetCurrent() - start
            return (totalTime * 1000) / Double(iterations)
        }

        return -1 // No benchmark function available
    }

    /// Get list of exported functions (for debugging)
    func listExports() -> [String] {
        return exportedFunctions
    }
}
