//
// RuvectorBridge.swift
// VibeCheck
//
// Swift wrapper around Ruvector WASM for on-device learning
//

import Foundation
import WasmKit

/// Bridge between VibeCheck and Ruvector WASM recommendation engine
///
/// Provides privacy-preserving on-device learning with 8M+ ops/sec performance.
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
    
    enum RuvectorError: Error {
        case wasmNotLoaded
        case invalidPath
        case loadFailed(String)
        case functionNotFound(String)
        case conversionFailed
    }
    
    // MARK: - Properties
    
    private var wasmModule: WasmKit.Module?
    private var wasmInstance: WasmKit.Instance?
    private var memory: WasmKit.Memory?
    
    private let embeddingDim: Int
    private let numActions: Int
    
    public private(set) var isReady: Bool = false
    
    // MARK: - Initialization
    
    public init(embeddingDim: Int = 64, numActions: Int = 100) {
        self.embeddingDim = embeddingDim
        self.numActions = numActions
    }
    
    // MARK: - Lifecycle
    
    /// Load the WASM module
    public func load(wasmPath: String) async throws {
        guard FileManager.default.fileExists(atPath: wasmPath) else {
            throw RuvectorError.invalidPath
        }
        
        do {
            // Load WASM bytes
            let wasmData = try Data(contentsOf: URL(fileURLWithPath: wasmPath))
            
            // Create WasmKit engine
            let engine = Engine()
            let store = Store(engine: engine)
            
            // Compile module
            self.wasmModule = try Module(store: store, bytes: Array(wasmData))
            
            // Create imports (WASI support)
            var imports = Imports()
            
            // Instantiate
            self.wasmInstance = try wasmModule!.instantiate(store: store, imports: imports)
            
            // Get memory reference
            if let memoryExport = wasmInstance!.exports.first(where: { $0.name == "memory" }) {
                self.memory = memoryExport.value as? WasmKit.Memory
            }
            
            // Initialize Ruvector
            try await callInit()
            
            self.isReady = true
            
        } catch {
            throw RuvectorError.loadFailed(error.localizedDescription)
        }
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
        for (index, genre) in item.genres.enumerated() where index < 32 {
            categoryFlags |= (1 << index)
        }
        
        // Popularity: Normalized value (0.0 - 1.0)
        let popularity: Float = 0.5  // Default, can be enriched later
        
        // Recency: Based on year
        let currentYear = Calendar.current.component(.year, from: Date())
        let yearDiff = max(0, currentYear - item.year)
        let recency = Float(max(0, 10 - yearDiff)) / 10.0  // More recent = higher score
        
        return ContentMetadata(
            id: id,
            contentType: 0,  // Video
            durationSecs: UInt32(item.runtime * 60),
            categoryFlags: categoryFlags,
            popularity: popularity,
            recency: recency
        )
    }
    
    //MARK: - Learning
    
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
        
        // Set current vibe
        try await setVibe(vibeState)
        
        // Embed content
        try await embedContent(content)
        
        // Calculate satisfaction score based on watch duration
        let expectedDuration = Double(item.runtime * 60)
        let watchRatio = min(1.0, Double(durationSeconds) / expectedDuration)
        let satisfaction = Float(watchRatio)
        
        // Update learning (Q-learning)
        try await updateLearning(
            contentId: content.id,
            interactionType: durationSeconds > 0 ? 4 : 3,  // 4=complete, 3=skip
            timeSpent: satisfaction,
            position: 0
        )
    }
    
    /// Learn from user interaction
    public func learn(satisfaction: Double) async throws {
        guard isReady else {
            throw RuvectorError.wasmNotLoaded
        }
        
        // Propagate reward signal
        // This reinforces recent interactions
        // (Implementation depends on Ruvector's API)
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
        
        // Set current vibe
        let vibeState = mapToVibeState(context)
        try await setVibe(vibeState)
        
        // Get recommendations from WASM
        let contentIds = try await getRecommendationsWASM(limit: limit)
        
        // Convert IDs back to MediaItems
        // (This requires a local content database - simplified for now)
        return contentIds.compactMap { id in
            // Lookup MediaItem by ID
            // For now, return empty array - needs integration with LocalStore
            return nil
        }
    }
    
    // MARK: - Persistence
    
    /// Save learned state to Data
    public func saveState() async throws -> Data {
        guard isReady else {
            throw RuvectorError.wasmNotLoaded
        }
        
        // Call WASM save_state function
        if let saveFunc = try? wasmInstance?.export(function: "save_state") {
            let ptr = try saveFunc.call()
            
            // Read from WASM memory
            if let ptr = ptr as? Int,
               let memory = self.memory {
                // Read size from first 4 bytes
                let size = memory.data.withUnsafeBytes { bytes in
                    bytes.load(fromByteOffset: ptr, as: UInt32.self)
                }
                
                // Read data
                let data = memory.data.subdata(in: ptr + 4..< ptr + 4 + Int(size))
                return data
            }
        }
        
        return Data()
    }
    
    /// Load saved state from Data
    public func loadState(_ data: Data) async throws {
        guard isReady else {
            throw RuvectorError.wasmNotLoaded
        }
        
        // Allocate memory in WASM
        // Copy data to WASM memory
        // Call WASM load_state function
        
        if let loadFunc = try? wasmInstance?.export(function: "load_state") {
            // Write data to WASM memory
            if let memory = self.memory {
                // Allocate space
                let ptr = memory.data.count
                memory.data.append(data)
                
                // Call load function
                try loadFunc.call(Int32(ptr), UInt32(data.count))
            }
        }
    }
    
    // MARK: - Private WASM Calls
    
    private func callInit() async throws {
        guard let initFunc = try? wasmInstance?.export(function: "init") else {
            throw RuvectorError.functionNotFound("init")
        }
        
        try initFunc.call(UInt32(embeddingDim), UInt32(numActions))
    }
    
    private func setVibe(_ state: VibeState) async throws {
        guard let setVibeFunc = try? wasmInstance?.export(function: "set_vibe") else {
            throw RuvectorError.functionNotFound("set_vibe")
        }
        
        try setVibeFunc.call(
            state.energy,
            state.mood,
            state.focus,
            state.timeContext,
            state.preferences.0,
            state.preferences.1,
            state.preferences.2,
            state.preferences.3
        )
    }
    
    private func embedContent(_ content: ContentMetadata) async throws {
        guard let embedFunc = try? wasmInstance?.export(function: "embed_content") else {
            throw RuvectorError.functionNotFound("embed_content")
        }
        
        try embedFunc.call(
            content.id,
            content.contentType,
            content.durationSecs,
            content.categoryFlags,
            content.popularity,
            content.recency
        )
    }
    
    private func updateLearning(
        contentId: UInt64,
        interactionType: UInt8,
        timeSpent: Float,
        position: UInt8
    ) async throws {
        guard let updateFunc = try? wasmInstance?.export(function: "update_learning") else {
            throw RuvectorError.functionNotFound("update_learning")
        }
        
        try updateFunc.call(
            contentId,
            interactionType,
            timeSpent,
            position
        )
    }
    
    private func getRecommendationsWASM(limit: Int) async throws -> [UInt64] {
        guard let getRecsFunc = try? wasmInstance?.export(function: "get_recommendations") else {
            throw RuvectorError.functionNotFound("get_recommendations")
        }
        
        // Call function (returns pointer to ID array)
        let result = try getRecsFunc.call(UInt32(limit))
        
        // Parse result from WASM memory
        if let ptr = result as? Int,
           let memory = self.memory {
            var ids: [UInt64] = []
            
            for i in 0..<limit {
                let offset = ptr + (i * 8)  // 8 bytes per UInt64
                let id = memory.data.withUnsafeBytes { bytes in
                    bytes.load(fromByteOffset: offset, as: UInt64.self)
                }
                ids.append(id)
            }
            
            return ids
        }
        
        return []
    }
}

// MARK: - WasmKit Extensions

extension WasmKit.Instance {
    func export(function name: String) throws -> WasmKit.Function {
        guard let exportedFunc = self.exports.first(where: { $0.name == name }),
              let function = exportedFunc.value as? WasmKit.Function else {
            throw RuvectorBridge.RuvectorError.functionNotFound(name)
        }
        return function
    }
}
