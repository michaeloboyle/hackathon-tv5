import SwiftUI

struct MoodMeshBackground: View {
    let mood: MoodState

    var body: some View {
        TimelineView(.animation(minimumInterval: 1/30)) { timeline in
            if #available(iOS 18.0, *) {
                MeshGradient(
                    width: 3,
                    height: 3,
                    points: animatedPoints(for: timeline.date),
                    colors: moodColors
                )
                .ignoresSafeArea()
            } else {
                // Fallback for iOS 17
                LinearGradient(
                    colors: [moodColors.first ?? .blue, moodColors.last ?? .purple],
                    startPoint: .topLeading,
                    endPoint: .bottomTrailing
                )
                .ignoresSafeArea()
            }
        }
    }

    private func animatedPoints(for date: Date) -> [SIMD2<Float>] {
        let t = Float(date.timeIntervalSinceReferenceDate)
        let drift = Float(0.03)

        return [
            SIMD2(0, 0),
            SIMD2(0.5 + sin(t * 0.4) * drift, 0),
            SIMD2(1, 0),
            SIMD2(0 + cos(t * 0.3) * drift, 0.5),
            SIMD2(0.5 + sin(t * 0.5) * drift, 0.5 + cos(t * 0.4) * drift),
            SIMD2(1 - cos(t * 0.35) * drift, 0.5),
            SIMD2(0, 1),
            SIMD2(0.5 + sin(t * 0.45) * drift, 1),
            SIMD2(1, 1)
        ]
    }

    private var moodColors: [Color] {
        return AppTheme.colors(for: mood.recommendationHint)
    }
}

#Preview {
    MoodMeshBackground(mood: .chill)
}
