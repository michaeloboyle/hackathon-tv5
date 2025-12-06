import Foundation
import Observation

@Observable
class RecommendationEngine {

    var recommendations: [MediaItem] = []
    var isLoading = false

    private let catalog: [MediaItem]

    init(catalog: [MediaItem] = MediaItem.samples) {
        self.catalog = catalog
    }

    /// Generate recommendations based on mood and preferences
    func generateRecommendations(
        mood: MoodState,
        preferences: UserPreferences,
        limit: Int = 5
    ) -> [MediaItem] {

        let hint = mood.recommendationHint

        // Step 1: Filter by mood hint
        var candidates = catalog.filter { item in
            matchesMoodHint(item: item, hint: hint)
        }

        // Step 2: Filter by user preferences
        candidates = candidates.filter { item in
            // Exclude avoided genres
            let hasAvoidedGenre = item.genres.contains { preferences.avoidGenres.contains($0) }
            if hasAvoidedGenre { return false }

            // Exclude avoided titles
            if preferences.avoidTitles.contains(item.id) { return false }

            // Filter by available platforms
            if !preferences.subscriptions.isEmpty {
                let hasAvailablePlatform = item.platforms.contains { preferences.subscriptions.contains($0) }
                if !hasAvailablePlatform { return false }
            }

            // Filter by runtime preferences
            if let minRuntime = preferences.preferredMinRuntime, item.runtime < minRuntime {
                return false
            }
            if let maxRuntime = preferences.preferredMaxRuntime, item.runtime > maxRuntime {
                return false
            }

            return true
        }

        // Step 3: Score remaining candidates
        let scored = candidates.map { item -> (MediaItem, Double) in
            let score = scoreItem(item: item, mood: mood, preferences: preferences)
            return (item, score)
        }

        // Step 4: Sort by score and limit
        let sorted = scored.sorted { $0.1 > $1.1 }
        return Array(sorted.prefix(limit).map { $0.0 })
    }

    // MARK: - Mood Matching

    private func matchesMoodHint(item: MediaItem, hint: String) -> Bool {
        switch hint {
        case "comfort":
            // Feel-good, familiar, rewatchable content
            return item.tone.contains("feel-good") ||
                   item.tone.contains("heartwarming") ||
                   item.isRewatch ||
                   (item.genres.contains("comedy") && item.intensity < 0.5)

        case "gentle":
            // Low intensity, not stressful
            return item.intensity < 0.4 &&
                   !item.genres.contains("thriller") &&
                   !item.genres.contains("horror") &&
                   !item.tone.contains("intense")

        case "light":
            // Comedies, short content, easy watching
            return item.genres.contains("comedy") ||
                   item.tone.contains("light") ||
                   item.runtime < 35

        case "engaging":
            // Moderate intensity, interesting but not exhausting
            return item.intensity >= 0.4 &&
                   item.intensity < 0.7 &&
                   !item.tone.contains("slow")

        case "exciting":
            // High energy, action-packed
            return item.genres.contains("action") ||
                   item.genres.contains("adventure") ||
                   item.tone.contains("exciting") ||
                   item.intensity >= 0.7

        case "calming":
            // Slow, meditative content
            return item.tone.contains("slow") ||
                   item.tone.contains("calm") ||
                   item.genres.contains("documentary") ||
                   item.intensity < 0.3

        default: // "balanced"
            return true
        }
    }

    // MARK: - Scoring

    private func scoreItem(
        item: MediaItem,
        mood: MoodState,
        preferences: UserPreferences
    ) -> Double {
        var score = 0.0

        // Boost for favorite genres
        let favoriteMatches = item.genres.filter { preferences.favoriteGenres.contains($0) }.count
        score += Double(favoriteMatches) * 0.3

        // Boost for rating
        if let rating = item.rating {
            score += (rating - 7.0) * 0.2 // Boost for ratings above 7
        }

        // Boost for rewatch comfort when mood is comfort
        if mood.recommendationHint == "comfort" && item.isRewatch {
            score += 0.5
        }

        // Boost for appropriate runtime based on energy
        switch mood.energy {
        case .exhausted, .low:
            if item.runtime < 40 { score += 0.3 }
        case .high, .wired:
            if item.runtime > 90 { score += 0.2 }
        default:
            break
        }

        // Boost for recent content
        let currentYear = Calendar.current.component(.year, from: Date())
        if item.year >= currentYear - 1 {
            score += 0.2
        }

        return score
    }

    // MARK: - Public API

    func refresh(mood: MoodState, preferences: UserPreferences) {
        // Legacy support mapping MoodState to a query if VibeContext isn't available
        let query = buildQuery(from: mood)
        refresh(query: query, mood: mood, preferences: preferences)
    }
    
    func refresh(context: VibeContext, preferences: UserPreferences) {
        // Use the smart keywords from VibePredictor
        let query = context.keywords.joined(separator: " ") + " " + context.explanation
        refresh(query: query, mood: context.mood, preferences: preferences)
    }

    private func refresh(query: String, mood: MoodState, preferences: UserPreferences) {
        isLoading = true
        
        Task {
            // 1. Get local Rule-Based recommendations (fast, safety net)
            let ruleRecs = generateRecommendations(mood: mood, preferences: preferences)
            
            // 2. Get Semantic Vector recommendations (The Sommelier)
            // Filter catalog first by hard constraints to avoid searching things we can't watch? 
            // Actually, better to search all then filter.
            let semanticRecs = VectorEmbeddingService.shared.search(
                query: query,
                in: catalog
            )
            
            // 3. Filter semantic recs by preferences (subscriptions, excluded genres)
            let filteredSemantic = semanticRecs.filter { item in
                // Exclude avoided genres/titles
                if item.genres.contains(where: { preferences.avoidGenres.contains($0) }) { return false }
                if preferences.avoidTitles.contains(item.id) { return false }
                
                // Platform check
                if !preferences.subscriptions.isEmpty {
                     if !item.platforms.contains(where: { preferences.subscriptions.contains($0) }) { return false }
                }
                return true
            }
            
            // 4. Merge (Interleave: Semantic First, then Rule Based)
            // De-duplicate
            var seenIds = Set<String>()
            var merged: [MediaItem] = []
            
            let maxCount = max(filteredSemantic.count, ruleRecs.count)
            for i in 0..<maxCount {
                if i < filteredSemantic.count {
                    let item = filteredSemantic[i]
                    if !seenIds.contains(item.id) {
                        merged.append(item)
                        seenIds.insert(item.id)
                    }
                }
                if i < ruleRecs.count {
                    let item = ruleRecs[i]
                    if !seenIds.contains(item.id) {
                        merged.append(item)
                        seenIds.insert(item.id)
                    }
                }
            }
            
            await MainActor.run {
                self.recommendations = Array(merged.prefix(10))
                self.isLoading = false
            }
        }
    }
    
    private func buildQuery(from mood: MoodState) -> String {
        // "I feel [tired] and [stressed]. Show me [comfort] movies."
        var query = "I feel \(mood.energy) energy and \(mood.stress) stress."
        query += " Show me \(mood.recommendationHint) movies and TV shows."
        return query
    }
}
