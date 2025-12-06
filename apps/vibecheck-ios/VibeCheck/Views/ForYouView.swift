import SwiftUI
import SwiftData

struct ForYouView: View {
    @Environment(\.modelContext) private var modelContext
    @State private var healthManager = HealthKitManager()
    @State private var engine = RecommendationEngine()
    @State private var currentMood: MoodState?
    @State private var vibeContext: VibeContext?
    @State private var selectedItem: MediaItem?
    @State private var showingHealthPermission = false

    private let vibePredictor = VibePredictor()
    
    // ... (rest of the view)


    var body: some View {
        NavigationStack {
            ZStack {
                // Dynamic mesh gradient background
                if let mood = currentMood {
                    MoodMeshBackground(mood: mood)
                        .opacity(0.6)
                }

                ScrollView {
                    LazyVStack(spacing: 24) {
                        // Header section
                        VibeHeader(mood: currentMood, isLoading: healthManager.isLoading)
                            .padding(.top, 20)

                        // Vibe ring
                        if let mood = currentMood {
                            VibeRing(mood: mood)
                                .padding(.vertical, 10)
                        } else if !healthManager.isLoading {
                            // Prompt to check vibe
                            Button {
                                Task { await refresh() }
                            } label: {
                                Label("Check My Vibe", systemImage: "waveform.path.ecg")
                                    .font(.headline)
                                    .padding()
                                    .background(.ultraThinMaterial, in: Capsule())
                            }
                            .buttonStyle(.plain)
                            .padding(.vertical, 20)
                        }

                        // Quick mood override
                        if currentMood != nil {
                            QuickMoodOverride { newMood in
                                withAnimation(.spring) {
                                    currentMood = newMood
                                    logMood(newMood)
                                }
                                refreshRecommendations()
                            }
                            .padding(.horizontal)
                        }

                        // Recommendations section
                        if !engine.recommendations.isEmpty {
                            Section {
                                ForEach(engine.recommendations) { item in
                                    RecommendationCard(item: item, mood: currentMood ?? .default)
                                        .onTapGesture {
                                            selectedItem = item
                                        }
                                        .scrollTransition { content, phase in
                                            content
                                                .opacity(phase.isIdentity ? 1 : 0.7)
                                                .scaleEffect(phase.isIdentity ? 1 : 0.96)
                                                .blur(radius: phase.isIdentity ? 0 : 1)
                                        }
                                }
                            } header: {
                                sectionHeader
                            }
                            .padding(.horizontal)
                        }

                        // Privacy note
                        privacyNote
                            .padding(.top, 20)
                            .padding(.bottom, 40)
                    }
                }
                .scrollIndicators(.hidden)
                .refreshable {
                    await refresh()
                }
            }
            .navigationTitle("For You")
            .navigationBarTitleDisplayMode(.large)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        Task { await refresh() }
                    } label: {
                        if #available(iOS 18.0, *) {
                            Image(systemName: "arrow.trianglehead.2.clockwise.rotate.90")
                                .symbolEffect(.rotate, value: healthManager.isLoading)
                        } else {
                            Image(systemName: "arrow.trianglehead.2.clockwise.rotate.90")
                                .symbolEffect(.variableColor, isActive: healthManager.isLoading)
                        }
                    }
                }
            }
            .sheet(item: $selectedItem) { item in
                MediaDetailSheet(item: item, mood: currentMood ?? .default)
                    .presentationDetents([.medium, .large])
                    .presentationDragIndicator(.visible)
                    .presentationCornerRadius(32)
            }
            .alert("Health Access", isPresented: $showingHealthPermission) {
                Button("Open Settings", role: .none) {
                    if let url = URL(string: UIApplication.openSettingsURLString) {
                        UIApplication.shared.open(url)
                    }
                }
                Button("Not Now", role: .cancel) { }
            } message: {
                Text("Enable Health access in Settings to get personalized recommendations based on your current state.")
            }
            .task {
                await initialize()
            }
        }
    }

    // MARK: - Subviews

    private var sectionHeader: some View {
        HStack {
            Text("For Your \(currentMood?.recommendationHint.capitalized ?? "") Mood")
                .font(.title3)
                .fontWeight(.bold)
            Spacer()
        }
        .padding(.top, 8)
    }

    private var privacyNote: some View {
        HStack(spacing: 8) {
            Image(systemName: "lock.shield.fill")
                .foregroundStyle(.green)

            Text("Your health data never leaves this device")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding(.horizontal)
    }

    // MARK: - Actions

    private func initialize() async {
        guard healthManager.isHealthDataAvailable else {
            // Use default mood for simulator/devices without HealthKit
            currentMood = .default
            refreshRecommendations()
            return
        }

        do {
            try await healthManager.requestAuthorization()
            await refresh()
        } catch {
            print("HealthKit authorization failed: \(error)")
            showingHealthPermission = true
            // Fall back to default mood
            currentMood = .default
            refreshRecommendations()
        }
    }

    private func refresh() async {
        await healthManager.fetchCurrentContext()

        // Use the new VibePredictor (Checking biometrics...)
        let context = vibePredictor.predictVibe(
            hrv: healthManager.currentHRV,
            sleepHours: healthManager.lastSleepHours,
            steps: healthManager.stepsToday
        )

        withAnimation(.spring) {
            currentMood = context.mood
            vibeContext = context
        }

        logMood(context.mood)
        refreshRecommendations()
    }

    private func refreshRecommendations() {
        guard let context = vibeContext else { return }
        
        // Ensure engine uses the semantic context
        let preferences = UserPreferences.default
        engine.refresh(context: context, preferences: preferences)
    }

    private func logMood(_ mood: MoodState) {
        let log = MoodLog(
            mood: mood,
            hrv: healthManager.currentHRV,
            sleepHours: healthManager.lastSleepHours,
            steps: healthManager.stepsToday
        )
        modelContext.insert(log)
    }
}

// MARK: - Media Detail Sheet

struct MediaDetailSheet: View {
    let item: MediaItem
    let mood: MoodState
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                // Hero
                ZStack(alignment: .bottomLeading) {
                    Rectangle()
                        .fill(
                            LinearGradient(
                                colors: [.purple, .blue],
                                startPoint: .topLeading,
                                endPoint: .bottomTrailing
                            )
                        )
                        .frame(height: 200)

                    LinearGradient(
                        colors: [.clear, Color(.systemBackground)],
                        startPoint: .top,
                        endPoint: .bottom
                    )
                    .frame(height: 100)

                    VStack(alignment: .leading, spacing: 4) {
                        Text(item.title)
                            .font(.largeTitle)
                            .fontWeight(.bold)

                        HStack {
                            Text(String(item.year))
                            Text("•")
                            Text(item.formattedRuntime)
                            if let rating = item.rating {
                                Text("•")
                                Label(String(format: "%.1f", rating), systemImage: "star.fill")
                            }
                        }
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                    }
                    .padding()
                }

                VStack(alignment: .leading, spacing: 16) {
                    // Why this recommendation
                    HStack(spacing: 8) {
                        Image(systemName: "waveform.path.ecg")
                            .foregroundStyle(.green)
                        Text("Recommended because you're in \(mood.recommendationHint) mode")
                            .font(.subheadline)
                    }
                    .padding()
                    .background(.green.opacity(0.1), in: RoundedRectangle(cornerRadius: 12))

                    // Overview
                    if !item.overview.isEmpty {
                        Text(item.overview)
                            .font(.body)
                            .foregroundStyle(.secondary)
                    }

                    // Platforms
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Available on")
                            .font(.headline)

                        HStack(spacing: 12) {
                            ForEach(item.platforms, id: \.self) { platform in
                                HStack {
                                    PlatformBadge(platform: platform)
                                    Text(platform.capitalized)
                                        .font(.subheadline)
                                }
                                .padding(.horizontal, 12)
                                .padding(.vertical, 8)
                                .background(.quaternary, in: Capsule())
                            }
                        }
                    }

                    // Genres
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Genres")
                            .font(.headline)

                        FlowLayout(spacing: 8) {
                            ForEach(item.genres, id: \.self) { genre in
                                Text(genre.capitalized)
                                    .font(.subheadline)
                                    .padding(.horizontal, 12)
                                    .padding(.vertical, 6)
                                    .background(.quaternary, in: Capsule())
                            }
                        }
                    }

                    // Tone tags
                    if !item.tone.isEmpty {
                        VStack(alignment: .leading, spacing: 8) {
                            Text("Vibe")
                                .font(.headline)

                            FlowLayout(spacing: 8) {
                                ForEach(item.tone, id: \.self) { tone in
                                    Text(tone.capitalized)
                                        .font(.subheadline)
                                        .padding(.horizontal, 12)
                                        .padding(.vertical, 6)
                                        .background(.blue.opacity(0.1), in: Capsule())
                                }
                            }
                        }
                    }
                }
                .padding()
            }
        }
    }
}

// MARK: - Flow Layout

struct FlowLayout: Layout {
    var spacing: CGFloat = 8

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let result = FlowResult(in: proposal.width ?? 0, subviews: subviews, spacing: spacing)
        return result.size
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let result = FlowResult(in: bounds.width, subviews: subviews, spacing: spacing)
        for (index, subview) in subviews.enumerated() {
            subview.place(at: CGPoint(x: bounds.minX + result.positions[index].x,
                                      y: bounds.minY + result.positions[index].y),
                         proposal: .unspecified)
        }
    }

    struct FlowResult {
        var size: CGSize = .zero
        var positions: [CGPoint] = []

        init(in maxWidth: CGFloat, subviews: Subviews, spacing: CGFloat) {
            var x: CGFloat = 0
            var y: CGFloat = 0
            var rowHeight: CGFloat = 0

            for subview in subviews {
                let size = subview.sizeThatFits(.unspecified)

                if x + size.width > maxWidth, x > 0 {
                    x = 0
                    y += rowHeight + spacing
                    rowHeight = 0
                }

                positions.append(CGPoint(x: x, y: y))
                rowHeight = max(rowHeight, size.height)
                x += size.width + spacing
            }

            self.size = CGSize(width: maxWidth, height: y + rowHeight)
        }
    }
}

#Preview {
    ForYouView()
        .modelContainer(for: [WatchHistory.self, UserPreferences.self, MoodLog.self], inMemory: true)
}
