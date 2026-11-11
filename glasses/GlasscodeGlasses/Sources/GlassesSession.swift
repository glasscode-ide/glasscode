import Foundation
import Combine
import MWDATCore
import MWDATDisplay
import MWDATInputs
import MWDATSpeech

/// A neural-band / touchpad event, normalized away from the DAT `InputEvent` zoo
/// into the handful of things the browse UI actually cares about.
enum GlassesInput: Equatable { case up, down, left, right, select, back, capture }

// Owns the one `DeviceSession` to the Meta Ray-Ban Display glasses and layers the
// three capabilities the Glasscode flow needs on top of it:
//
//   • Display — renders whatever screen the flow hands it (`show`).
//   • Inputs  — neural band / touchpad nav, forwarded as `GlassesInput` via `onInput`.
//   • Speech  — Meta's on-device transcription, started on demand for Voice mode.
//
// Display attaches automatically on first render; Inputs attaches once the session
// is started; Speech attaches lazily the first time Voice mode asks to listen.
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
    @Published var isListening = false

    /// Neural-band / touchpad events, already normalized.
    var onInput: ((GlassesInput) -> Void)?
    /// Partial + final transcriptions while Speech is running (Voice mode).
    var onTranscript: ((TranscriptionResult) -> Void)?
    /// Surfaced when Speech can't start (permission denied, unavailable).
    var onSpeechError: ((String) -> Void)?
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

    // Speech
    private var speech: Speech?
    private var speechStateToken: AnyListenerToken?
    private var speechTranscriptToken: AnyListenerToken?
    private var speechErrorToken: AnyListenerToken?

    private var coreStateTask: Task<Void, Never>?
    private var errorTask: Task<Void, Never>?

    init(wearables: WearablesInterface = Wearables.shared) {
        self.wearables = wearables
        self.selector = AutoDeviceSelector(wearables: wearables, filter: { $0.supportsDisplay() })
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
            if deviceSession == nil { attach() }
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
            sendToLens(GlasscodeScreens.blank())
        }
        onDisplayChange?()
    }

    /// Back-compat for the old passive HUD readout.
    func render(_ hud: GlasscodeHUD) { show(GlassesHUD.screen(hud)) }

    func connect() { if deviceSession == nil { attach() } }

    func disconnect() {
        display?.stop()
        deviceSession?.stop()
        teardown()
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
                    case .started: self.requiresDATAppUpdate = false; await self.setupDisplay(session)
                    case .stopping: self.isConnected = false
                    case .stopped: self.teardown()
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
                        self.attachInputs(session)
                        if let action = self.pending { self.pending = nil; await action() }
                    case .stopping: self.isConnected = false
                    case .stopped:
                        self.isConnected = false; self.display = nil
                        self.deviceSession?.stop()
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
        // Navigation is swipes-only (swipe-right = open). The pinch is free, so it toggles
        // the lens on/off. The action button and capture press stay unused — just logged.
        case .select(let src, _):
            toggleDisplay()
            lastEvent = "pinch · \(src) — lens \(displayOn ? "on" : "off")"
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

    // MARK: - Speech (Voice mode, on-device transcription)

    /// Ask for the mic (once, on a user action) and start Meta's on-device transcription.
    func startSpeech() {
        guard let session = deviceSession else { onSpeechError?("Glasses aren't connected."); return }
        Task { @MainActor in
            // Check first — only *requesting* launches Meta AI and backgrounds us (which drops
            // the glasses session). Once granted, later Voice uses never leave the app.
            let current = (try? await Wearables.shared.checkPermissionStatus(.microphone)) ?? .denied
            if current != .granted {
                let granted = (try? await Wearables.shared.requestPermission(.microphone)) ?? .denied
                guard granted == .granted else { self.onSpeechError?("Turn on Microphone for Glasscode in Meta AI."); return }
            }
            if self.speech == nil {
                do {
                    guard let sp = try session.addSpeech() else { self.onSpeechError?("Transcription unavailable."); return }
                    self.speech = sp
                    self.speechStateToken = sp.statePublisher.listen { [weak self] st in
                        Task { @MainActor in self?.isListening = (st == .started) }
                    }
                    self.speechTranscriptToken = sp.transcriptionPublisher.listen { [weak self] result in
                        Task { @MainActor in self?.onTranscript?(result) }
                    }
                    self.speechErrorToken = sp.errorPublisher.listen { [weak self] _ in
                        Task { @MainActor in self?.onSpeechError?("Transcription stopped."); self?.isListening = false }
                    }
                } catch {
                    self.onSpeechError?("Couldn't start transcription."); return
                }
            }
            self.speech?.start()
            self.isListening = true
        }
    }

    func stopSpeech() {
        speech?.stop()
        isListening = false
    }

    // MARK: - teardown

    private func teardown() {
        isConnected = false
        inputsAvailable = false
        isListening = false
        sendInFlight = false
        queuedView = nil

        displayStateToken = nil
        displayStateContinuation?.finish(); displayStateContinuation = nil
        displayStateTask?.cancel(); displayStateTask = nil

        inputsEventTask?.cancel(); inputsEventTask = nil
        inputsStateToken = nil; inputsErrorToken = nil
        if inputs != nil { try? deviceSession?.removeInputs(); inputs = nil }

        speechStateToken = nil; speechTranscriptToken = nil; speechErrorToken = nil
        if speech != nil { speech?.stop(); try? deviceSession?.removeSpeech(); speech = nil }

        coreStateTask?.cancel(); coreStateTask = nil
        errorTask?.cancel(); errorTask = nil
        display = nil
        deviceSession = nil
        pending = nil
        selector = AutoDeviceSelector(wearables: wearables, filter: { $0.supportsDisplay() })
    }
}
