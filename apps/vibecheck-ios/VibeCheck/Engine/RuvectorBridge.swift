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
///
/// Usage:
/// ```swift
/// let bridge = RuvectorBridge()
/// try await bridge.load(wasmPath: Bundle.main.path(forResource: "ruvector", ofType: "wasm")!)
///
/// // Record watch event
/// try await bridge.recordWatchEvent(mediaItem, context: vibeContext, durationSeconds: 3600)
///
/// // Get personalized recommendations
/// let recs = try await bridge.getRecommendations(for: vibeContext, limit: 10)
/// ```
@available(iOS 15.0, *)
public class RuvectorBridge {

    // MARK: - Types

    /// Ruvector's vibe state (from their API)
    struct VibeState {
        var energy: Float      // 0.0 = calm, 1.0 = energetic
        var mood: Float        // -1.0 = negative, 1.0 = positive
        var focus: Float       // 0.0 = relaxed, 1.0 = focused
        var timeContext: Float // 0.0 = morning, 1.0 = night
        var preferences: (Float, Float, Float, Float)
    }

    struct ContentMetadata {
        let id: UInt64
        let contentType: UInt8  // 0 = video
        let durationSecs: UInt32
        let categoryFlags: UInt32
        let popularity: Float
        let recency: Float
    }

    enum RuvectorError: Error, LocalizedError {
        case wasmNotLoaded
        case invalidPath
        case loadFailed(String)
        case functionNotFound(String)
        case conversionFailed
        case memoryNotFound
        case instantiationFailed(String)

        var errorDescription: String? {
            switch self {
            case .wasmNotLoaded: return "WASM module not loaded"
            case .invalidPath: return "Invalid WASM file path"
            case .loadFailed(let msg): return "Failed to load WASM: \(msg)"
            case .functionNotFound(let name): return "Function '\(name)' not found in WASM exports"
            case .conversionFailed: return "Type conversion failed"
            case .memoryNotFound: return "WASM memory export not found"
            case .instantiationFailed(let msg): return "WASM instantiation failed: \(msg)"
            }
        }
    }

    // MARK: - Properties

    private var engine: Engine?
    private var store: Store?
    private var wasmModule: Module?
    private var wasmInstance: Instance?

    private let embeddingDim: Int
    private let numActions: Int

    /// Whether the WASM module is loaded and ready
    public private(set) var isReady: Bool = false

    /// Time taken to load the WASM module (for benchmarking)
    public private(set) var loadTimeMs: Double = 0

    /// List of exported function names (for debugging)
    public private(set) var exportedFunctions: [String] = []

    // MARK: - Initialization

    public init(embeddingDim: Int = 64, numActions: Int = 100) {
        self.embeddingDim = embeddingDim
        self.numActions = numActions
    }

    // MARK: - Lifecycle

    /// Load the WASM module from a file path
    ///
    /// - Parameter wasmPath: Path to the .wasm file
    /// - Throws: RuvectorError if loading fails
    public func load(wasmPath: String) async throws {
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

            // Try to initialize if init function exists
            if hasExportedFunction(name: "init") {
                try callInit()
            }

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
    public func loadFromBundle() async throws {
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

    /// Get exported memory
    private func getExportedMemory() -> Memory? {
        guard let instance = wasmInstance else { return nil }
        if case .memory(let memory) = instance.export("memory") {
            return memory
        }
        return nil
    }

    // MARK: - Context Mapping

    /// Map VibeCheck's VibeContext to Ruvector's VibeState
    func mapToVibeState(_ context: VibeContext) -> VibeState {
        // Energy: 0.0 (low) to 1.0 (high)
        let energy: Float = {
            switch context.mood.energy {
            case .low: return 0.2
            case .balanced: return 0.5
            case .high: return 0.8
            }
        }()

        // Mood: -1.0 (stressed/negative) to 1.0 (positive)
        let mood: Float = {
            switch context.mood.stress {
            case .high: return -0.5  // High stress = negative mood
            case .balanced: return 0.0
            case .low: return 0.5   // Low stress = positive mood
            }
        }()

        // Focus: Based on energy and stress combination
        let focus: Float = {
            if context.mood.energy == .high && context.mood.stress == .low {
                return 0.8  // High energy, low stress = good focus
            } else if context.mood.stress == .high {
                return 0.3  // High stress = poor focus
            } else {
                return 0.5  // Balanced
            }
        }()

        // Time context: 0.0 (morning) to 1.0 (night)
        let hour = Calendar.current.component(.hour, from: Date())
        let timeContext = Float(hour) / 24.0

        // Preferences: Extract from keywords (simplified)
        let preferences: (Float, Float, Float, Float) = (0, 0, 0, 0)

        return VibeState(
            energy: energy,
            mood: mood,
            focus: focus,
            timeContext: timeContext,
            preferences: preferences
        )
    }

    /// Convert MediaItem to Ruvector ContentMetadata
    private func mapToContentMetadata(_ item: MediaItem, context: VibeContext) -> ContentMetadata {
        // Generate stable ID from string ID
        let id = UInt64(item.id.hashValue).magnitude

        // Category flags: Encode genres as bit flags
        var categoryFlags: UInt32 = 0
        for (index, _) in item.genres.enumerated() where index < 32 {
            categoryFlags |= (1 << index)
        }

        // Popularity: Normalized value (0.0 - 1.0)
        let popularity: Float = Float(item.rating ?? 5.0) / 10.0

        // Recency: Based on year
        let currentYear = Calendar.current.component(.year, from: Date())
        let yearDiff = max(0, currentYear - item.year)
        let recency = Float(max(0, 10 - yearDiff)) / 10.0

        return ContentMetadata(
            id: id,
            contentType: 0,  // Video
            durationSecs: UInt32(item.runtime * 60),
            categoryFlags: categoryFlags,
            popularity: popularity,
            recency: recency
        )
    }

    // MARK: - Learning

    /// Record a watch event for learning
    public func recordWatchEvent(
        _ item: MediaItem,
        context: VibeContext,
        durationSeconds: Int
    ) async throws {
        guard isReady else {
            throw RuvectorError.wasmNotLoaded
        }

        // Map to Ruvector types
        let vibeState = mapToVibeState(context)
        let content = mapToContentMetadata(item, context: context)

        // Set current vibe if function exists
        if hasExportedFunction(name: "set_vibe") {
            try setVibe(vibeState)
        }

        // Embed content if function exists
        if hasExportedFunction(name: "embed_content") {
            try embedContent(content)
        }

        // Calculate satisfaction score based on watch duration
        let expectedDuration = Double(item.runtime * 60)
        let watchRatio = min(1.0, Double(durationSeconds) / expectedDuration)
        let satisfaction = Float(watchRatio)

        // Update learning (Q-learning) if function exists
        if hasExportedFunction(name: "update_learning") {
            try updateLearning(
                contentId: content.id,
                interactionType: durationSeconds > 0 ? 4 : 3,
                timeSpent: satisfaction,
                position: 0
            )
        }
    }

    /// Learn from user interaction
    public func learn(satisfaction: Double) async throws {
        guard isReady else {
            throw RuvectorError.wasmNotLoaded
        }

        // Propagate reward signal if learn function exists
        if let learnFunc = getExportedFunction(name: "learn") {
            _ = try learnFunc.invoke([.f32(Float(satisfaction))])
        }
    }

    // MARK: - Recommendations

    /// Get personalized recommendations
    public func getRecommendations(
        for context: VibeContext,
        limit: Int
    ) async throws -> [MediaItem] {
        guard isReady else {
            throw RuvectorError.wasmNotLoaded
        }

        // Set current vibe if function exists
        if hasExportedFunction(name: "set_vibe") {
            let vibeState = mapToVibeState(context)
            try setVibe(vibeState)
        }

        // Get recommendations from WASM if function exists
        if let getRecsFunc = getExportedFunction(name: "get_recommendations") {
            let result = try getRecsFunc.invoke([.i32(Int32(limit))])

            // Parse result - returns pointer to content IDs
            if let firstResult = result.first, case .i32(let ptr) = firstResult {
                let contentIds = try readContentIds(fromPointer: Int(ptr), count: limit)

                // Convert IDs back to MediaItems from local catalog
                return contentIds.compactMap { id in
                    MediaItem.samples.first { UInt64($0.id.hashValue).magnitude == id }
                }
            }
        }

        return []
    }

    // MARK: - Persistence

    /// Save learned state to Data
    public func saveState() async throws -> Data {
        guard isReady else {
            throw RuvectorError.wasmNotLoaded
        }

        if let saveFunc = getExportedFunction(name: "save_state") {
            let result = try saveFunc.invoke([])

            if let firstResult = result.first,
               case .i32(let ptr) = firstResult,
               let memory = getExportedMemory() {
                // Read size from first 4 bytes
                var sizeBytes = [UInt8](repeating: 0, count: 4)
                try memory.read(offset: Int(ptr), into: &sizeBytes)
                let size = UInt32(littleEndian: sizeBytes.withUnsafeBytes { $0.load(as: UInt32.self) })

                // Read data
                var dataBytes = [UInt8](repeating: 0, count: Int(size))
                try memory.read(offset: Int(ptr) + 4, into: &dataBytes)
                return Data(dataBytes)
            }
        }

        return Data()
    }

    /// Load saved state from Data
    public func loadState(_ data: Data) async throws {
        guard isReady else {
            throw RuvectorError.wasmNotLoaded
        }

        if let loadFunc = getExportedFunction(name: "load_state"),
           let memory = getExportedMemory(),
           let allocFunc = getExportedFunction(name: "alloc") {
            // Allocate memory in WASM
            let allocResult = try allocFunc.invoke([.i32(Int32(data.count))])
            guard let firstResult = allocResult.first,
                  case .i32(let ptr) = firstResult else {
                return
            }

            // Write data to WASM memory
            try memory.write(offset: Int(ptr), bytes: Array(data))

            // Call load function
            _ = try loadFunc.invoke([.i32(ptr), .i32(Int32(data.count))])
        }
    }

    // MARK: - Benchmarking

    /// Benchmark WASM module loading
    public static func benchmarkLoad(wasmPath: String) async -> (success: Bool, timeMs: Double, error: String?) {
        let bridge = RuvectorBridge()
        do {
            try await bridge.load(wasmPath: wasmPath)
            return (true, bridge.loadTimeMs, nil)
        } catch {
            return (false, 0, error.localizedDescription)
        }
    }

    /// Benchmark a simple WASM operation
    public func benchmarkSimpleOp(iterations: Int = 1000) throws -> Double {
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
                _ = try addFunc.invoke([.i32(Int32(i)), .i32(Int32(i + 1))])
            }

            let totalTime = CFAbsoluteTimeGetCurrent() - start
            return (totalTime * 1000) / Double(iterations)
        }

        return -1 // No benchmark function available
    }

    /// Get list of exported functions (for debugging)
    public func listExports() -> [String] {
        return exportedFunctions
    }

    // MARK: - Private WASM Calls

    private func callInit() throws {
        guard let initFunc = getExportedFunction(name: "init") else {
            return // init is optional
        }
        _ = try initFunc.invoke([.i32(Int32(embeddingDim)), .i32(Int32(numActions))])
    }

    private func setVibe(_ state: VibeState) throws {
        guard let setVibeFunc = getExportedFunction(name: "set_vibe") else {
            return
        }
        _ = try setVibeFunc.invoke([
            .f32(state.energy),
            .f32(state.mood),
            .f32(state.focus),
            .f32(state.timeContext),
            .f32(state.preferences.0),
            .f32(state.preferences.1),
            .f32(state.preferences.2),
            .f32(state.preferences.3)
        ])
    }

    private func embedContent(_ content: ContentMetadata) throws {
        guard let embedFunc = getExportedFunction(name: "embed_content") else {
            return
        }
        _ = try embedFunc.invoke([
            .i64(Int64(content.id)),
            .i32(Int32(content.contentType)),
            .i32(Int32(content.durationSecs)),
            .i32(Int32(content.categoryFlags)),
            .f32(content.popularity),
            .f32(content.recency)
        ])
    }

    private func updateLearning(
        contentId: UInt64,
        interactionType: UInt8,
        timeSpent: Float,
        position: UInt8
    ) throws {
        guard let updateFunc = getExportedFunction(name: "update_learning") else {
            return
        }
        _ = try updateFunc.invoke([
            .i64(Int64(contentId)),
            .i32(Int32(interactionType)),
            .f32(timeSpent),
            .i32(Int32(position))
        ])
    }

    private func readContentIds(fromPointer ptr: Int, count: Int) throws -> [UInt64] {
        guard let memory = getExportedMemory() else {
            throw RuvectorError.memoryNotFound
        }

        var ids: [UInt64] = []

        for i in 0..<count {
            let offset = ptr + (i * 8)
            var bytes = [UInt8](repeating: 0, count: 8)
            try memory.read(offset: offset, into: &bytes)
            let id = bytes.withUnsafeBytes { $0.load(as: UInt64.self) }
            if id != 0 {
                ids.append(id)
            }
        }

        return ids
    }
}
