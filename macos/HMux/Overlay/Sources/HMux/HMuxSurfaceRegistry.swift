import Foundation

final class HMuxSurfaceRegistry {
    static let shared = HMuxSurfaceRegistry()

    private final class WeakSurface {
        weak var value: Ghostty.SurfaceView?
        init(_ value: Ghostty.SurfaceView) { self.value = value }
    }

    private var surfaces: [UUID: WeakSurface] = [:]

    func register(_ surface: Ghostty.SurfaceView) {
        compact()
        surfaces[surface.id] = WeakSurface(surface)
    }

    func unregister(_ surface: Ghostty.SurfaceView) {
        surfaces.removeValue(forKey: surface.id)
    }

    func surface(id: UUID) -> Ghostty.SurfaceView? {
        compact()
        return surfaces[id]?.value
    }

    private func compact() {
        surfaces = surfaces.filter { $0.value.value != nil }
    }
}
