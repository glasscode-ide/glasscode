import AVFoundation
import MWDATCore
import SwiftUI

/// The look the lens and the phone share (`LensPalette`, `docs/lens-everywhere.md`).
private enum Look {
    static let ground = Color(uiColor: LensPalette.ground)
    static let panel = Color(uiColor: LensPalette.panel)
    static let text = Color(uiColor: LensPalette.text)
    static let soft = Color(uiColor: LensPalette.soft)
    static let mint = Color(uiColor: LensPalette.color(.ok))
    static func tone(_ tone: LensModel.Tone) -> Color { Color(uiColor: LensPalette.color(tone)) }
}

/// The phone's screen (G14, `docs/lens-everywhere.md`): one view, edge to edge. On top who is
/// connected; in the middle the place the Mac's window shows, as the lens shows it but with the
/// room a phone has (a row opens on the Mac and the lens together); the places to go to; and a
/// big talk button. Pairing and settings are sheets.
struct HomeView: View {
    @ObservedObject var app: AppModel
    @ObservedObject var link: ManagerLink
    @ObservedObject var glasses: GlassesSession
    @StateObject private var registration = Registration()
    @State private var pairing = false
    @State private var settings = false
    @State private var talk = false
    @State private var terminal: String?
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        ZStack {
            backdrop
            if link.status == .unpaired { welcome } else { content }
        }
        .preferredColorScheme(.dark)
        .sheet(isPresented: $pairing) { PairView(link: link, done: { pairing = false; app.start() }) }
        .sheet(isPresented: $settings) { SettingsView(app: app, link: link, glasses: glasses, registration: registration) }
        .sheet(isPresented: $talk) { ConversationView(app: app) }
        .sheet(item: $terminal) { id in NavigationStack { SessionView(app: app, link: link, id: id) } }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { link.nudge(); glasses.resume() }
        }
    }

    /// Near black, with the place's colour glowing in from the top.
    private var backdrop: some View {
        ZStack {
            Look.ground
            RadialGradient(colors: [Look.tone(app.place?.tone ?? .ok).opacity(0.22), .clear], center: .top, startRadius: 10, endRadius: 520)
        }
        .ignoresSafeArea()
        .animation(.easeInOut(duration: 0.6), value: app.place?.tone)
    }

    private var welcome: some View {
        VStack(spacing: 22) {
            Orb(tone: .ok, active: true).frame(width: 120, height: 120)
            Text("Your manager, on your glasses").font(.system(.title, design: .rounded).weight(.semibold)).foregroundStyle(Look.text).multilineTextAlignment(.center)
            Text("On your Mac: Glasscode › Settings › Glasses › Pair glasses, then scan its code here. The phone and the Mac need Tailscale.")
                .font(.callout).foregroundStyle(Look.soft).multilineTextAlignment(.center)
            Button { pairing = true } label: { Text("Pair with my Mac").font(.headline).frame(maxWidth: .infinity).padding(.vertical, 14) }
                .background(Look.mint, in: Capsule()).foregroundStyle(.black)
        }
        .padding(28)
    }

    private var content: some View {
        VStack(spacing: 0) {
            topBar.padding(.horizontal, 16).padding(.top, 4)
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    live
                    approvals
                    if let page = app.place { PageView(page: page, open: { place in Task { await app.openPlace(place) } }, terminal: page.kind == "session" ? { terminal = page.place } : nil) }
                    else { PageView(page: LensPage(kind: "today", place: "today", eyebrow: link.paired?.mac.name ?? "Mac", title: link.status == .connected ? "Looking at your Mac…" : "Not connected", body: link.status == .connected ? "" : "The phone reaches your Mac over Tailscale."), open: { _ in }, terminal: nil) }
                }
                .padding(.horizontal, 16)
                .padding(.top, 10)
                .padding(.bottom, 24)
            }
            .refreshable { await app.refresh() }
            places
            talkBar
        }
    }

    // MARK: parts

    private var topBar: some View {
        HStack(spacing: 10) {
            Orb(tone: app.place?.tone ?? .plain, active: link.status == .connected).frame(width: 30, height: 30)
            VStack(alignment: .leading, spacing: 1) {
                Text(link.home?.name ?? "Glasscode").font(.system(.headline, design: .rounded)).foregroundStyle(Look.text)
                Text(statusLine).font(.caption2).foregroundStyle(Look.soft)
            }
            Spacer()
            chip(link.macConnected ? "Mac" : "Mac off", on: link.macConnected)
            chip(glasses.isConnected ? "Glasses" : "No glasses", on: glasses.isConnected)
            Button { settings = true } label: { Image(systemName: "slider.horizontal.3").font(.body.weight(.semibold)).foregroundStyle(Look.soft).frame(width: 34, height: 34) }
        }
    }

    private var statusLine: String {
        switch link.status {
        case .connected: return link.home?.place == .server ? "On the server" : "On \(link.paired?.mac.name ?? "your Mac")"
        case .connecting: return "Connecting…"
        case .unpaired: return "Not paired"
        case .unreachable: return "Can't reach your Mac"
        }
    }

    private func chip(_ label: String, on: Bool) -> some View {
        HStack(spacing: 5) {
            Circle().fill(on ? Look.mint : Look.soft.opacity(0.5)).frame(width: 6, height: 6).shadow(color: on ? Look.mint : .clear, radius: 3)
            Text(label).font(.caption2.weight(.medium)).foregroundStyle(on ? Look.text : Look.soft)
        }
        .padding(.horizontal, 9).padding(.vertical, 5)
        .background(Look.panel.opacity(0.8), in: Capsule())
        .overlay(Capsule().stroke(Look.soft.opacity(0.18), lineWidth: 1))
    }

    /// What the lens says over the page right now: listening, thinking, the answer.
    @ViewBuilder private var live: some View {
        let words: (String, String)? = {
            switch app.lens.card {
            case .listening(let heard): return ("Listening", heard.isEmpty ? "Say what you want…" : heard)
            case .working(let lines): return ("Thinking", lines.last ?? "")
            case .answer(let text, _): return ("Answer", text)
            case .note(let title, let body, _, _): return (title, body)
            case .message(let title, let body): return (title, body)
            default: return nil
            }
        }()
        if !app.writing.isEmpty || words != nil {
            VStack(alignment: .leading, spacing: 6) {
                Text((words?.0 ?? "Answer").uppercased()).font(.caption2.weight(.bold)).tracking(1.2).foregroundStyle(Look.tone(.busy))
                Text(app.writing.isEmpty ? (words?.1 ?? "") : app.writing).font(.body).foregroundStyle(Look.text)
            }
            .padding(14).frame(maxWidth: .infinity, alignment: .leading)
            .background(Look.panel.opacity(0.85), in: RoundedRectangle(cornerRadius: 18, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 18, style: .continuous).stroke(Look.tone(.busy).opacity(0.35), lineWidth: 1))
        }
    }

    @ViewBuilder private var approvals: some View {
        ForEach(Array((app.today?.array("approvals") ?? []).enumerated()), id: \.offset) { _, a in
            VStack(alignment: .leading, spacing: 10) {
                Text("NEEDS YOUR OK").font(.caption2.weight(.bold)).tracking(1.2).foregroundStyle(Look.tone(.warn))
                Text(a.string("what") ?? "").font(.headline).foregroundStyle(Look.text)
                Text(a.string("payload").flatMap { $0.isEmpty ? nil : $0 } ?? a.string("command") ?? "").font(.footnote.monospaced()).foregroundStyle(Look.soft)
                HStack(spacing: 10) {
                    Button { decide(a, true) } label: { Text("Allow").font(.subheadline.weight(.semibold)).frame(maxWidth: .infinity).padding(.vertical, 10) }
                        .background(Look.mint, in: Capsule()).foregroundStyle(.black)
                    Button { decide(a, false) } label: { Text("Deny").font(.subheadline.weight(.semibold)).frame(maxWidth: .infinity).padding(.vertical, 10) }
                        .overlay(Capsule().stroke(Look.soft.opacity(0.5), lineWidth: 1)).foregroundStyle(Look.text)
                }
            }
            .padding(16).frame(maxWidth: .infinity, alignment: .leading)
            .background(Look.panel, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 20, style: .continuous).stroke(Look.tone(.warn).opacity(0.45), lineWidth: 1))
        }
    }

    private func decide(_ approval: JSON, _ allow: Bool) {
        guard let id = approval.int("id") else { return }
        let launch = app.today?.string("launch") ?? ""
        Task { try? await link.decide(approval: id, allow: allow, launch: launch, shown: "phone"); await app.refresh() }
    }

    /// The places to go to: each opens on the Mac and the lens together.
    private var places: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach([("Today", "today", "sun.max"), ("Sessions", "sessions", "rectangle.stack"), ("Tickets", "tickets", "checklist"), ("Pull requests", "pull-requests", "arrow.triangle.pull"), ("Usage", "usage", "gauge.with.dots.needle.33percent")], id: \.1) { label, place, symbol in
                    let here = app.place?.place == place
                    Button { Task { await app.openPlace(place) } } label: {
                        Label(label, systemImage: symbol).font(.footnote.weight(.semibold)).padding(.horizontal, 12).padding(.vertical, 8)
                            .foregroundStyle(here ? .black : Look.text)
                            .background(here ? AnyShapeStyle(Look.mint) : AnyShapeStyle(Look.panel), in: Capsule())
                            .overlay(Capsule().stroke(Look.soft.opacity(here ? 0 : 0.2), lineWidth: 1))
                    }
                    .disabled(!link.macConnected)
                }
            }
            .padding(.horizontal, 16)
        }
        .padding(.vertical, 8)
    }

    /// The talk button, and the band's moves for when there is no band.
    private var talkBar: some View {
        HStack(spacing: 14) {
            pad("chevron.left") { app.band(.left) }
            pad("chevron.up") { app.band(.up) }
            Button {
                if app.turn.isListening { app.band(.select) } else { app.talk() }
            } label: {
                ZStack {
                    Orb(tone: app.voiceMuted ? .plain : app.talkOpen || app.turn.isListening ? .busy : .ok, active: (app.talkOpen && !app.voiceMuted) || app.turn.isListening).frame(width: 74, height: 74)
                    Image(systemName: app.voiceMuted ? "mic.slash.fill" : app.talkOpen || app.turn.isListening ? "waveform" : "mic.fill").font(.title2.weight(.semibold)).foregroundStyle(.black)
                }
            }
            .disabled(link.status != .connected)
            pad("chevron.down") { app.band(.down) }
            pad("chevron.right") { app.band(.right) }
        }
        .padding(.top, app.talkOpen ? 44 : 4)
        .padding(.bottom, 6)
        .frame(maxWidth: .infinity)
        .overlay(alignment: .topTrailing) {
            Button { talk = true } label: { Image(systemName: "text.bubble").font(.body.weight(.semibold)).foregroundStyle(Look.soft).frame(width: 40, height: 40) }
                .padding(.trailing, 12)
        }
        // While a talk is open: make it silent (or let it listen again), or end the talk.
        .overlay(alignment: .top) {
            if app.talkOpen {
                HStack(spacing: 10) {
                    Button { app.setMuted(!app.voiceMuted) } label: {
                        Label(app.voiceMuted ? "Silent · tap to talk" : "Make it silent", systemImage: app.voiceMuted ? "mic.slash.fill" : "mic.slash")
                            .font(.footnote.weight(.semibold)).padding(.horizontal, 12).padding(.vertical, 7)
                            .foregroundStyle(app.voiceMuted ? .black : Look.text)
                            .background(app.voiceMuted ? AnyShapeStyle(Look.tone(.warn)) : AnyShapeStyle(Look.panel), in: Capsule())
                            .overlay(Capsule().stroke(Look.soft.opacity(app.voiceMuted ? 0 : 0.25), lineWidth: 1))
                    }
                    Button { app.endTalk() } label: {
                        Label("End talk", systemImage: "phone.down.fill")
                            .font(.footnote.weight(.semibold)).padding(.horizontal, 12).padding(.vertical, 7)
                            .foregroundStyle(Look.tone(.bad))
                            .background(Look.panel, in: Capsule())
                            .overlay(Capsule().stroke(Look.tone(.bad).opacity(0.4), lineWidth: 1))
                    }
                }
            }
        }
    }

    private func pad(_ symbol: String, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol).font(.body.weight(.semibold)).foregroundStyle(Look.text).frame(width: 44, height: 44)
                .background(Look.panel, in: Circle()).overlay(Circle().stroke(Look.soft.opacity(0.2), lineWidth: 1))
        }
    }
}

/// A place's page, with the room a phone has: what the lens shows, larger, every row at once.
struct PageView: View {
    let page: LensPage
    let open: (String) -> Void
    /// A session's terminal, view only.
    let terminal: (() -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Text(page.eyebrow.isEmpty ? page.kind.capitalized : page.eyebrow).font(.caption.weight(.semibold)).tracking(0.8).foregroundStyle(Look.soft)
                Spacer()
                if !page.status.isEmpty {
                    Text(page.status).font(.caption.weight(.bold)).foregroundStyle(Look.tone(page.tone))
                        .padding(.horizontal, 10).padding(.vertical, 4)
                        .background(Look.tone(page.tone).opacity(0.14), in: Capsule())
                        .overlay(Capsule().stroke(Look.tone(page.tone).opacity(0.5), lineWidth: 1))
                }
            }
            Text(page.title).font(.system(.title2, design: .rounded).weight(.bold)).foregroundStyle(Look.text)
            if let meter = page.meter {
                GeometryReader { geo in
                    ZStack(alignment: .leading) {
                        Capsule().fill(Look.soft.opacity(0.2))
                        Capsule().fill(Look.tone(page.tone)).frame(width: max(8, geo.size.width * meter.value)).shadow(color: Look.tone(page.tone).opacity(0.8), radius: 6)
                    }
                }
                .frame(height: 8)
                Text(meter.label).font(.footnote).foregroundStyle(Look.soft)
            } else {
                LinearGradient(colors: [Look.tone(page.tone), Look.tone(page.tone).opacity(0)], startPoint: .leading, endPoint: .trailing).frame(height: 2)
            }
            if !page.body.isEmpty {
                Text(page.body).font(.callout).foregroundStyle(Look.text.opacity(0.85)).textSelection(.enabled)
            }
            if let terminal {
                Button(action: terminal) { Label("Terminal and message", systemImage: "terminal").font(.footnote.weight(.semibold)).foregroundStyle(Look.tone(.busy)) }
            }
            VStack(spacing: 8) {
                ForEach(Array(page.items.enumerated()), id: \.offset) { _, item in
                    Button { if let place = item.open { open(place) } } label: { row(item) }
                        .buttonStyle(.plain)
                        .allowsHitTesting(item.open != nil)
                }
            }
            if page.total > page.items.count {
                Text("\(page.total - page.items.count) more on the Mac").font(.footnote).foregroundStyle(Look.soft)
            }
            HStack {
                if let prev = page.prev { Button { open(prev) } label: { Label("Previous", systemImage: "chevron.up") } }
                Spacer()
                if let next = page.next { Button { open(next) } label: { Label("Next", systemImage: "chevron.down") } }
            }
            .font(.footnote.weight(.semibold)).foregroundStyle(Look.mint)
        }
        .padding(18)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Look.panel.opacity(0.92), in: RoundedRectangle(cornerRadius: 24, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 24, style: .continuous).stroke(LinearGradient(colors: [Look.tone(page.tone).opacity(0.6), Look.soft.opacity(0.08)], startPoint: .topLeading, endPoint: .bottomTrailing), lineWidth: 1))
    }

    private func row(_ item: LensPage.Item) -> some View {
        HStack(spacing: 12) {
            Circle().fill(Look.tone(item.tone)).frame(width: 9, height: 9).shadow(color: Look.tone(item.tone).opacity(item.tone == .plain ? 0 : 0.9), radius: 4)
            VStack(alignment: .leading, spacing: 2) {
                Text(item.title).font(.subheadline.weight(.semibold)).foregroundStyle(Look.text).lineLimit(2)
                if !item.detail.isEmpty { Text(item.detail).font(.caption).foregroundStyle(Look.soft).lineLimit(2) }
            }
            Spacer(minLength: 0)
            if item.open != nil { Image(systemName: "chevron.right").font(.caption.weight(.bold)).foregroundStyle(Look.soft) }
        }
        .padding(.horizontal, 12).padding(.vertical, 10)
        .background(Look.ground.opacity(0.55), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
    }
}

/// The status orb, as on the lens: a bright core in a halo, breathing while it is active.
struct Orb: View {
    let tone: LensModel.Tone
    let active: Bool
    @State private var breathe = false

    var body: some View {
        let color = Look.tone(tone)
        ZStack {
            Circle().fill(RadialGradient(colors: [color.opacity(0.55), color.opacity(0)], center: .center, startRadius: 2, endRadius: 60)).scaleEffect(breathe ? 1.12 : 0.92)
            Circle().stroke(color, lineWidth: 2).padding(6)
            Circle().fill(color).padding(14).shadow(color: color, radius: 8)
        }
        .onAppear { if active { withAnimation(.easeInOut(duration: 1.6).repeatForever(autoreverses: true)) { breathe = true } } }
        .onChange(of: active) { _, on in withAnimation(on ? .easeInOut(duration: 1.6).repeatForever(autoreverses: true) : .default) { breathe = on } }
    }
}

/// The conversation with the manager, in full.
struct ConversationView: View {
    @ObservedObject var app: AppModel

    var body: some View {
        ScrollView {
            VStack(spacing: 10) {
                ForEach(Array(app.conversation.suffix(60).enumerated()), id: \.offset) { _, item in
                    let mine = (item["author"] as? String) == "person"
                    Text(item.string("text") ?? "")
                        .font(.callout).foregroundStyle(Look.text)
                        .padding(12)
                        .frame(maxWidth: .infinity, alignment: mine ? .trailing : .leading)
                        .background((mine ? Look.mint.opacity(0.16) : Look.panel), in: RoundedRectangle(cornerRadius: 16, style: .continuous))
                }
            }
            .padding(16)
        }
        .background(Look.ground.ignoresSafeArea())
        .preferredColorScheme(.dark)
        .presentationDragIndicator(.visible)
    }
}

extension String: @retroactive Identifiable {
    public var id: String { self }
}

/// One session: its last reply, its question, a message to it, and its terminal, view only.
struct SessionView: View {
    @ObservedObject var app: AppModel
    @ObservedObject var link: ManagerLink
    let id: String
    @State private var screen = ""
    @State private var message = ""

    var body: some View {
        let card = app.sessionCard(id)
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                if let card {
                    Text(card.state.uppercased()).font(.caption2.weight(.bold)).tracking(1.2).foregroundStyle(Look.tone(card.tone))
                    if let question = card.question { Text("Asks: \(question)").font(.headline).foregroundStyle(Look.text) }
                    if !card.lastReply.isEmpty { Text(card.lastReply).font(.callout).foregroundStyle(Look.text.opacity(0.85)) }
                }
                HStack {
                    TextField("Say something to this session", text: $message).textFieldStyle(.roundedBorder)
                    Button("Send") {
                        let text = message
                        message = ""
                        Task { try? await link.send(session: id, text: text) }
                    }
                    .disabled(message.isEmpty)
                }
                Text("TERMINAL · VIEW ONLY").font(.caption2.weight(.bold)).tracking(1.2).foregroundStyle(Look.soft)
                Text(screen.isEmpty ? "…" : screen).font(.caption2.monospaced()).foregroundStyle(Look.text).frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10).background(Look.panel, in: RoundedRectangle(cornerRadius: 10))
            }
            .padding(16)
        }
        .background(Look.ground.ignoresSafeArea())
        .navigationTitle(card?.title ?? "Session")
        .preferredColorScheme(.dark)
        .task {
            while !Task.isCancelled {
                screen = await link.peek(id)
                try? await Task.sleep(for: .seconds(2))
            }
        }
    }
}

/// Pairing: scan the Mac's code, or paste its link.
struct PairView: View {
    @ObservedObject var link: ManagerLink
    let done: () -> Void
    @State private var text = ""
    @State private var problem: String?
    @State private var working = false

    var body: some View {
        NavigationStack {
            VStack(spacing: 16) {
                QRScanner { found in if !working { pair(found) } }
                    .frame(height: 300)
                    .clipShape(RoundedRectangle(cornerRadius: 16))
                TextField("Or paste the pairing link", text: $text).textFieldStyle(.roundedBorder).autocorrectionDisabled().textInputAutocapitalization(.never)
                Button(working ? "Pairing…" : "Pair") { pair(text) }.buttonStyle(.borderedProminent).disabled(text.isEmpty || working)
                if let problem { Text(problem).font(.footnote).foregroundStyle(.orange) }
                Spacer()
            }
            .padding(20)
            .navigationTitle("Pair with your Mac")
        }
    }

    private func pair(_ found: String) {
        guard let pairing = PairingLink(found) else { problem = "That is not a Glasscode pairing code."; return }
        working = true
        problem = nil
        Task {
            do {
                try await link.pair(pairing)
                done()
            } catch {
                problem = error.localizedDescription
            }
            working = false
        }
    }
}

/// Settings: the glasses, the microphone, staying awake, and this phone's pairing.
struct SettingsView: View {
    @ObservedObject var app: AppModel
    @ObservedObject var link: ManagerLink
    @ObservedObject var glasses: GlassesSession
    @ObservedObject var registration: Registration
    @AppStorage(BackgroundKeepAlive.enabledKey) private var keepAlive = true
    @AppStorage(AudioRoute.micOnGlassesKey) private var micOnGlasses = false
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Form {
                Section("Glasses") {
                    LabeledContent("Status", value: glasses.statusText)
                    if registration.state == .registered {
                        Button(glasses.stayOn ? "Hand the glasses back to Meta" : "Keep the glasses on Glasscode") {
                            if glasses.stayOn { glasses.disconnect() } else { glasses.connect() }
                        }
                    } else {
                        Button("Connect the glasses (opens Meta AI)") { registration.register() }
                    }
                    if let why = glasses.inputsError { Text("Band: \(why)").font(.footnote).foregroundStyle(.secondary) }
                }
                Section("Voice") {
                    Toggle("Use the glasses' microphone", isOn: $micOnGlasses)
                    Toggle("Stay awake in the background", isOn: $keepAlive)
                        .onChange(of: keepAlive) { _, _ in BackgroundKeepAlive.shared.refresh() }
                }
                Section("Paired") {
                    if let paired = link.paired {
                        LabeledContent("Mac", value: paired.mac.name)
                        if let server = paired.server { LabeledContent("Server", value: server.name) }
                        LabeledContent("This phone", value: paired.device)
                    }
                    Button("Forget this pairing", role: .destructive) { link.unpair(); dismiss() }
                }
                Section("About") {
                    NavigationLink("Licences") { LicencesView() }
                }
            }
            .navigationTitle("Settings")
            .toolbar { Button("Done") { dismiss() } }
        }
    }
}

/// Tracks Meta's registration so the settings can offer Connect.
@MainActor
final class Registration: ObservableObject {
    @Published var state: RegistrationState = Wearables.shared.registrationState
    private var task: Task<Void, Never>?

    init() {
        task = Task { [weak self] in
            for await s in Wearables.shared.registrationStateStream() {
                guard let self else { return }
                self.state = s
            }
        }
    }

    deinit { task?.cancel() }

    func register() { Task { try? await Wearables.shared.startRegistration() } }
}

/// The camera, reading one QR code.
struct QRScanner: UIViewControllerRepresentable {
    let found: (String) -> Void

    func makeUIViewController(context: Context) -> ScannerController {
        let controller = ScannerController()
        controller.found = found
        return controller
    }

    func updateUIViewController(_ controller: ScannerController, context: Context) {}

    final class ScannerController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
        var found: ((String) -> Void)?
        private let session = AVCaptureSession()
        private var reported = false

        override func viewDidLoad() {
            super.viewDidLoad()
            view.backgroundColor = .black
            guard let camera = AVCaptureDevice.default(for: .video), let input = try? AVCaptureDeviceInput(device: camera), session.canAddInput(input) else { return }
            session.addInput(input)
            let output = AVCaptureMetadataOutput()
            guard session.canAddOutput(output) else { return }
            session.addOutput(output)
            output.setMetadataObjectsDelegate(self, queue: .main)
            output.metadataObjectTypes = [.qr]
            let preview = AVCaptureVideoPreviewLayer(session: session)
            preview.videoGravity = .resizeAspectFill
            preview.frame = view.bounds
            view.layer.addSublayer(preview)
            DispatchQueue.global(qos: .userInitiated).async { [session] in session.startRunning() }
        }

        override func viewDidLayoutSubviews() {
            super.viewDidLayoutSubviews()
            view.layer.sublayers?.first?.frame = view.bounds
        }

        override func viewWillDisappear(_ animated: Bool) {
            super.viewWillDisappear(animated)
            session.stopRunning()
        }

        func metadataOutput(_ output: AVCaptureMetadataOutput, didOutput objects: [AVMetadataObject], from connection: AVCaptureConnection) {
            guard !reported, let code = objects.compactMap({ $0 as? AVMetadataMachineReadableCodeObject }).first?.stringValue else { return }
            reported = true
            found?(code)
            DispatchQueue.main.asyncAfter(deadline: .now() + 3) { [weak self] in self?.reported = false }
        }
    }
}
