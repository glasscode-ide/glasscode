import Foundation
import Combine
import MWDATCore
import MWDATDisplay
import MWDATInputs

/// A neural-band / touchpad event, normalized away from the DAT `InputEvent` zoo
/// into the handful of things the browse UI actually cares about.
enum GlassesInput: Equatable { case up, down, left, right, select, back, capture }

// Owns the one `DeviceSession` to the Meta Ray-Ban Display glasses and layers the two
// capabilities the app needs on top of it:
//
//   • Display — renders whatever card the lens model hands it (`show`).
//   • Inputs  — the Neural Band and the touchpad, forwarded as `GlassesInput` via `onInput`:
//               a pinch talks or picks (G12), swipes move and open.
//
// Display attaches automatically on first render; Inputs attaches once the session is started.
// Speech is the phone's own (`Speech.swift`): Meta's on-glasses transcription is dev and beta
// only.
@MainActor
final class GlassesSession: ObservableObject {
    @Published var isConnected = false
    @Published var statusText = "Glasses not connected"
    @Published var requiresDATAppUpdate = false
    /// Lens on/off, toggled by the pinch. Off = desk mode (look at the IDE mirror), the
    /// lens shows a near-blank card but the band keeps driving us; on = mobile mode.
    @Published var displayOn = true
    /// False when the band couldn't be attached (not approved / older glasses) — the
    /// flow then falls back to the phone's on-screen nav pad.
    @Published var inputsAvailable = false
    /// Why Inputs didn't attach (shown on the phone so band problems are diagnosable).
    @Published var inputsError: String?
    /// Human-readable description of the last raw input event (shown under ⋯ for diagnosis).
    @Published var lastEvent = ""
    /// Stay on Glasscode. The glasses end our session for reasons the app cannot prevent
    /// (Meta AI, the back gesture, a tap and hold on the touchpad, a call); while this is on,
    /// a new session is started by itself. Only `disconnect()` — the person's own choice on
    /// the phone — switches it off and leaves the glasses to Meta.
    @Published private(set) var stayOn = true
    /// The wearer paused the session on the glasses (one tap on the touchpad). Another tap
    /// resumes it; the app cannot.
    @Published private(set) var paused = false

    /// Neural-band / touchpad events, already normalized.
    var onInput: ((GlassesInput) -> Void)?
    /// Fired when the pinch toggles the lens on/off, so the flow can republish the mirror.
    var onDisplayChange: (() -> Void)?

    private let wearables: WearablesInterface
    private var selector: DeviceSelector
    private var deviceSession: DeviceSession?

    // Display
    private var display: Display?
    private var displayStateToken: AnyListenerToken?
    private var displayStateTask: Task<Void, Never>?
    private var displayStateContinuation: AsyncStream<DisplayState>.Continuation?
    private var pending: (() async -> Void)?
    private var latest: FlexBox?

    // Inputs
    private var inputs: Inputs?
    private var inputsEventTask: Task<Void, Never>?
    private var inputsStateToken: AnyListenerToken?
    private var inputsErrorToken: AnyListenerToken?


    private var coreStateTask: Task<Void, Never>?
    private var errorTask: Task<Void, Never>?

    // Staying on Glasscode
    private var reconnectTask: Task<Void, Never>?
    /// How many tries in a row have not reached a started lens (sets the wait before the next).
    private var reconnectAttempt = 0
    private var lastStopAt = Date.distantPast
    /// When the lens was started again inside a living session (at most three a minute).
    private var displayRestarts: [Date] = []
    private var registrationToken: AnyListenerToken?
    private var devicesToken: AnyListenerToken?
    private var linkTokens: [AnyListenerToken] = []

    init(wearables: WearablesInterface = Wearables.shared) {
        self.wearables = wearables
        self.selector = AutoDeviceSelector(wearables: wearables, filter: { $0.supportsDisplay() })
        // Three things mean "try again now": the app was approved in Meta AI, a pair of
        // glasses was added, or the glasses came back in range.
        registrationToken = wearables.addRegistrationStateListener { [weak self] state in
            guard state == .registered else { return }
            Task { @MainActor in self?.resume() }
        }
        devicesToken = wearables.addDevicesListener { [weak self] ids in
            Task { @MainActor in self?.watchLinks(ids) }
        }
        watchLinks(wearables.devices)
    }

    private func watchLinks(_ ids: [DeviceIdentifier]) {
        linkTokens = ids.compactMap { id -> AnyListenerToken? in
            guard let device = wearables.deviceForIdentifier(id), device.supportsDisplay() else { return nil }
            return device.addLinkStateListener { [weak self] link in
                guard link == .connected else { return }
                Task { @MainActor in self?.resume() }
            }
        }
    }

    // MARK: - rendering

    /// Push a screen to the lens. Auto-attaches the display on first use; while the
    /// lens is warming up the newest screen is queued and sent once it reports `.started`.
    func show(_ view: FlexBox) {
        latest = view
        guard displayOn else { return }   // lens toggled dark; keep `latest` for when it returns
        sendToLens(view)
    }

    // Coalesced lens sender: while one frame is still rendering on the glasses, newer frames
    // overwrite the single queued one instead of piling up — so fast navigation jumps straight
    // to the latest selection with no backlog of stale frames to grind through.
    private var queuedView: FlexBox?
    private var sendInFlight = false

    private func sendToLens(_ view: FlexBox) {
        guard let display, isConnected else {
            pending = { [weak self] in
                guard let self, let d = self.display, let v = self.latest else { return }
                try? await d.send(v)
            }
            // Not connected: start (or keep waiting for) a session, unless the person
            // handed the glasses back to Meta.
            if deviceSession == nil, reconnectTask == nil { scheduleReconnect() }
            return
        }
        queuedView = view
        guard !sendInFlight else { return }   // a drain loop is running; it'll pick up queuedView
        sendInFlight = true
        Task { @MainActor [weak self] in
            guard let self else { return }
            while let v = self.queuedView {
                self.queuedView = nil
                try? await display.send(v)
            }
            self.sendInFlight = false
        }
    }

    /// The pinch toggles the lens. "Off" sends a near-blank card — the Display + Inputs
    /// capabilities stay attached so the neural band keeps driving the IDE mirror; "on"
    /// restores the current screen. Desk mode = off (look at the IDE tab), mobile = on.
    func toggleDisplay() {
        displayOn.toggle()
        if displayOn {
            if let v = latest { sendToLens(v) }
        } else {
            sendToLens(LensCards.blank())
        }
        onDisplayChange?()
    }

    /// The person wants the glasses on Glasscode (the Connect button, or opening the app).
    func connect() {
        stayOn = true
        reconnectAttempt = 0
        BackgroundKeepAlive.shared.refresh()
        guard deviceSession == nil else { return }
        scheduleReconnect()
    }

    /// The person hands the glasses back to Meta. Nothing reconnects until `connect()`.
    func disconnect() {
        stayOn = false
        reconnectTask?.cancel(); reconnectTask = nil
        display?.stop()
        deviceSession?.stop()
        teardown()
        statusText = "Glasses handed back to Meta"
        BackgroundKeepAlive.shared.refresh()
    }

    /// Something changed that makes a new try worthwhile (the app came to the front, the
    /// glasses came in range, the app was approved): try now instead of waiting out the pause.
    func resume() {
        guard stayOn, deviceSession == nil else { return }
        reconnectAttempt = 0
        scheduleReconnect()
    }

    // MARK: - staying on Glasscode

    /// Start a new session after a pause. The first try comes as soon as Meta allows (two
    /// seconds after the last session ended); every try that fails waits longer, up to a minute,
    /// so glasses that are in their case are not asked over and over.
    private func scheduleReconnect() {
        guard stayOn, deviceSession == nil else { return }
        reconnectTask?.cancel()
        let pauses: [TimeInterval] = [0, 4, 8, 15, 30, 60]
        let sinceStop = Date().timeIntervalSince(lastStopAt)
        let delay = max(0.3, 2.5 - sinceStop, pauses[min(reconnectAttempt, pauses.count - 1)])
        reconnectTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(delay))
            guard let self, !Task.isCancelled else { return }
            self.reconnectTask = nil
            guard self.stayOn, self.deviceSession == nil else { return }
            self.reconnectAttempt += 1
            // Not approved in Meta AI yet: nothing to connect to. The registration listener
            // tries again the moment it is.
            guard self.wearables.registrationState == .registered else { self.scheduleReconnect(); return }
            self.attach()
        }
    }

    /// The lens stopped while the session lives (a call, a notification, Meta AI on top of
    /// us). Start the lens again instead of giving the session up. Three tries a minute; if
    /// the lens still will not stay, end the session so a fresh one is made.
    private func restartDisplay(_ session: DeviceSession) {
        Task { [weak self] in
            try? await Task.sleep(for: .seconds(1))
            guard let self, self.deviceSession === session, let display = self.display else { return }
            // Stopping or stopped: the session handler makes a new one. Paused: wait for the
            // wearer to resume; the `.started` that follows comes back here.
            guard session.state == .started else { return }
            let now = Date()
            self.displayRestarts = self.displayRestarts.filter { now.timeIntervalSince($0) < 60 }
            guard self.displayRestarts.count < 3 else {
                self.displayRestarts = []
                session.stop()
                return
            }
            self.displayRestarts.append(now)
            display.start()
        }
    }

    // MARK: - session lifecycle

    private func attach() {
        statusText = "Connecting to glasses…"
        do {
            let session = try wearables.createSession(deviceSelector: selector)
            deviceSession = session
            let states = session.stateStream()
            let errors = session.errorStream()
            coreStateTask = Task { [weak self] in
                for await st in states {
                    guard let self, !Task.isCancelled else { return }
                    switch st {
                    case .started:
                        self.requiresDATAppUpdate = false
                        if self.paused {
                            self.paused = false
                            self.statusText = "Glasses connected"
                        }
                        if self.display == nil {
                            await self.setupDisplay(session)
                        } else if !self.isConnected {
                            self.restartDisplay(session)          // the lens went down while paused
                        } else if self.displayOn, let view = self.latest {
                            self.sendToLens(view)                 // back from a pause: show where we are
                        }
                    case .paused:
                        self.paused = true
                        self.statusText = "Paused on the glasses · tap the touchpad to resume"
                    case .stopping: self.isConnected = false
                    case .stopped:
                        self.statusText = self.stayOn ? "Glasses left Glasscode · reconnecting…" : "Glasses not connected"
                        self.teardown()
                    default: break
                    }
                }
            }
            errorTask = Task { [weak self] in
                for await e in errors {
                    guard let self, !Task.isCancelled else { return }
                    self.requiresDATAppUpdate = (e == .datAppOnTheGlassesUpdateRequired)
                    self.statusText = e.localizedDescription
                }
            }
            try session.start()
        } catch DeviceSessionError.datAppOnTheGlassesUpdateRequired {
            requiresDATAppUpdate = true; statusText = "Glasses app update required"; teardown()
        } catch {
            statusText = "Couldn't connect: \(error.localizedDescription)"; teardown()
        }
    }

    private func setupDisplay(_ session: DeviceSession) async {
        guard display == nil else { return }
        do {
            let cap = try session.addDisplay()
            let (stream, cont) = AsyncStream.makeStream(of: DisplayState.self)
            displayStateContinuation = cont
            displayStateToken = cap.statePublisher.listen { cont.yield($0) }
            displayStateTask = Task { [weak self] in
                for await st in stream {
                    guard let self, !Task.isCancelled else { return }
                    switch st {
                    case .started:
                        self.isConnected = true
                        self.statusText = "Glasses connected"
                        self.reconnectAttempt = 0
                        self.attachInputs(session)
                        if let action = self.pending {
                            self.pending = nil; await action()
                        } else if self.displayOn, let view = self.latest {
                            self.sendToLens(view)                 // the lens came back: redraw
                        }
                    case .stopping: self.isConnected = false
                    case .stopped:
                        // The session is kept: only the lens is started again.
                        self.isConnected = false
                        self.restartDisplay(session)
                    default: break
                    }
                }
            }
            cap.start()
            display = cap
        } catch {
            session.stop(); statusText = "Display failed: \(error.localizedDescription)"; teardown()
        }
    }

    // MARK: - Inputs (neural band)

    private func attachInputs(_ session: DeviceSession) {
        guard inputs == nil else { return }
        do {
            // Default config: all surfaces incl. the band + its drag stream, consumeBack: true.
            // (The band's swipes arrive as the `neuralBandDrag` stream, handled in handleDrag.)
            guard let inp = try session.addInputs() else {
                inputsAvailable = false; inputsError = "Band unavailable on these glasses."; return
            }
            inputs = inp
            inputsStateToken = inp.statePublisher.listen { _ in }
            inputsErrorToken = inp.errorPublisher.listen { [weak self] err in
                Task { @MainActor in
                    guard let self else { return }
                    self.inputsError = err.description
                    if err == .permissionDenied || err == .capabilityUnavailable { self.inputsAvailable = false }
                }
            }
            inputsEventTask = Task { [weak self] in
                guard let inp = self?.inputs else { return }
                for await event in inp.events {
                    guard let self, !Task.isCancelled else { return }
                    await MainActor.run { self.handleInputEvent(event) }
                }
            }
            inputsAvailable = true
            inputsError = nil
        } catch {
            inputsAvailable = false
            inputsError = "\(error)"
        }
    }

    // Accumulated band-drag travel; a drag of either axis past its net direction = one nav step.
    private var dragX: Float = 0
    private var dragY: Float = 0

    private func handleInputEvent(_ event: InputEvent) {
        switch event {
        case .nav(let direction, let src, _):
            lastEvent = "nav \(direction) · \(src)"
            switch direction {
            case .up: onInput?(.up); case .down: onInput?(.down)
            case .left: onInput?(.left); case .right: onInput?(.right)
            }
        // The pinch talks, or picks the focused row (G12). The action button and capture press
        // stay unused, just logged.
        case .select(let src, _):
            lastEvent = "pinch · \(src)"
            onInput?(.select)
        case .back(let src, _):
            lastEvent = "back · \(src)"; onInput?(.back)          // not delivered on HW, harmless
        case .button(_, let src, _):
            lastEvent = "button · \(src) — ignored"
        case .capture(let press, let src, _):
            lastEvent = "capture \(press) · \(src) — ignored"
        case .drag(let action, _, _, let dx, let dy, let src, _):
            handleDrag(action: action, dx: dx, dy: dy, src: src)
        }
    }

    // The band reports swipes as a drag stream (down → move… → up). We sum the travel and,
    // on lift, emit one nav step in the net direction — unit-agnostic, so it works whether the
    // SDK reports pixels or a normalized delta. (Flip the signs here if a swipe goes the wrong way.)
    private func handleDrag(action: DragAction, dx: Float, dy: Float, src: InputSource) {
        switch action {
        case .down:
            dragX = 0; dragY = 0
        case .move:
            dragX += dx; dragY += dy
            lastEvent = "drag move dx=\(round(dragX)) dy=\(round(dragY)) · \(src)"
        case .up:
            dragX += dx; dragY += dy
            lastEvent = "drag end dx=\(round(dragX)) dy=\(round(dragY)) · \(src)"
            let epsilon: Float = 0.0001
            if abs(dragY) >= abs(dragX), abs(dragY) > epsilon {
                onInput?(dragY > 0 ? .down : .up)
            } else if abs(dragX) > epsilon {
                onInput?(dragX > 0 ? .right : .left)
            }
            dragX = 0; dragY = 0
        }
    }

    // MARK: - teardown

    private func teardown() {
        if deviceSession != nil { lastStopAt = Date() }
        isConnected = false
        paused = false
        inputsAvailable = false
        sendInFlight = false
        queuedView = nil

        displayStateToken = nil
        displayStateContinuation?.finish(); displayStateContinuation = nil
        displayStateTask?.cancel(); displayStateTask = nil

        inputsEventTask?.cancel(); inputsEventTask = nil
        inputsStateToken = nil; inputsErrorToken = nil
        if inputs != nil { try? deviceSession?.removeInputs(); inputs = nil }

        coreStateTask?.cancel(); coreStateTask = nil
        errorTask?.cancel(); errorTask = nil
        display = nil
        deviceSession = nil
        pending = nil
        selector = AutoDeviceSelector(wearables: wearables, filter: { $0.supportsDisplay() })
        // Whatever ended the session, come back by ourselves unless the person disconnected.
        scheduleReconnect()
    }
}
