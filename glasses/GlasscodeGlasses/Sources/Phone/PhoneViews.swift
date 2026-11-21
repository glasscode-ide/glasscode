import AVFoundation
import MWDATCore
import SwiftUI

/// The phone's own screen (G14): the place for long text, the conversation in full, sessions,
/// cards and settings. The glasses are the place for everything else.
struct HomeView: View {
    @ObservedObject var app: AppModel
    @ObservedObject var link: ManagerLink
    @ObservedObject var glasses: GlassesSession
    @StateObject private var registration = Registration()
    @State private var pairing = false
    @State private var settings = false
    @State private var openSession: String?
    @Environment(\.scenePhase) private var scenePhase

    private let gold = Color(red: 0.95, green: 0.85, blue: 0.54)

    var body: some View {
        NavigationStack {
            ZStack {
                Color.black.ignoresSafeArea()
                if link.status == .unpaired {
                    welcome
                } else {
                    content
                }
            }
            .navigationTitle(link.home?.name ?? "Glasscode")
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button { settings = true } label: { Image(systemName: "gearshape") }
                }
            }
            .sheet(isPresented: $pairing) { PairView(link: link, done: { pairing = false; app.start() }) }
            .sheet(isPresented: $settings) { SettingsView(app: app, link: link, glasses: glasses, registration: registration) }
            .navigationDestination(item: $openSession) { id in SessionView(app: app, link: link, id: id) }
        }
        .preferredColorScheme(.dark)
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { link.nudge(); glasses.resume() }
        }
    }

    private var welcome: some View {
        VStack(spacing: 18) {
            Image(systemName: "eyeglasses").font(.system(size: 56)).foregroundStyle(gold)
            Text("Talk to your manager from your glasses").font(.title2.weight(.semibold)).multilineTextAlignment(.center)
            Text("On your Mac, open Glasscode › Settings › Glasses and choose Pair glasses. Then scan the code here. Your phone and your Mac need Tailscale.")
                .font(.callout).foregroundStyle(.secondary).multilineTextAlignment(.center)
            Button("Pair with my Mac") { pairing = true }.buttonStyle(.borderedProminent).tint(gold).foregroundStyle(.black)
        }
        .padding(28)
    }

    private var content: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                status
                lensMirror
                talkButton
                if !app.writing.isEmpty {
                    Text(app.writing).font(.body).padding(12).frame(maxWidth: .infinity, alignment: .leading).background(.white.opacity(0.06), in: RoundedRectangle(cornerRadius: 12))
                }
                cards
                sessionsList
                conversationList
            }
            .padding(16)
        }
        .refreshable { await app.refresh() }
    }

    private var status: some View {
        HStack(spacing: 10) {
            dot(link.macConnected ? .green : .gray)
            Text(link.paired?.mac.name ?? "Mac").font(.footnote)
            if link.paired?.server != nil {
                dot(link.serverConnected ? .green : .gray)
                Text(link.paired?.server?.name ?? "Server").font(.footnote)
            }
            Spacer()
            dot(glasses.isConnected ? .green : .orange)
            Text(glasses.isConnected ? "Glasses on" : "Glasses off").font(.footnote)
        }
        .foregroundStyle(.secondary)
    }

    private func dot(_ color: Color) -> some View {
        Circle().fill(color).frame(width: 8, height: 8)
    }

    /// What the lens shows, as words, with the band's moves for when there is no band.
    private var lensMirror: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(lensTitle).font(.headline).foregroundStyle(gold)
            Text(lensBody).font(.callout).foregroundStyle(.secondary).lineLimit(6)
            HStack(spacing: 10) {
                pad("chevron.up") { app.band(.up) }
                pad("chevron.down") { app.band(.down) }
                pad("chevron.left") { app.band(.left) }
                pad("chevron.right") { app.band(.right) }
                Spacer()
                pad("hand.pinch") { app.band(.select) }
            }
        }
        .padding(14)
        .background(.white.opacity(0.06), in: RoundedRectangle(cornerRadius: 14))
    }

    private func pad(_ symbol: String, _ action: @escaping () -> Void) -> some View {
        Button(action: action) { Image(systemName: symbol).frame(width: 36, height: 30) }.buttonStyle(.bordered)
    }

    private var lensTitle: String {
        switch app.lens.card {
        case .glance: return "\(app.lens.glance.name) · \(app.lens.glance.place)"
        case .listening: return "Listening"
        case .working: return "Thinking"
        case .answer: return "Answer"
        case .session(let s): return s.title
        case .pr(let pr): return "Pull request #\(pr.number)"
        case .points(let title, _, _): return title
        case .approval(let a): return a.what
        case .question(let q): return "\(q.title) asks"
        case .note(let title, _, _, _): return title
        case .message(let title, _): return title
        }
    }

    private var lensBody: String {
        switch app.lens.card {
        case .glance: return app.lens.glance.line
        case .listening(let heard): return heard.isEmpty ? "Say what you want…" : heard
        case .working(let lines): return lines.joined(separator: "\n")
        case .answer(let text, _): return text
        case .session(let s): return s.question.map { "Asks: \($0)" } ?? s.lastReply
        case .pr(let pr): return pr.state
        case .points(_, let points, let at): return points.indices.contains(at) ? points[at] : ""
        case .approval(let a): return a.payload
        case .question(let q): return q.question
        case .note(_, let body, _, _): return body
        case .message(_, let body): return body
        }
    }

    private var talkButton: some View {
        Button {
            if app.turn.isListening { app.band(.select) } else { app.talk() }
        } label: {
            Label(app.turn.isListening ? "Send" : "Talk to \(link.home?.name ?? "the manager")", systemImage: app.turn.isListening ? "paperplane.fill" : "mic.fill")
                .frame(maxWidth: .infinity).padding(.vertical, 6)
        }
        .buttonStyle(.borderedProminent).tint(gold).foregroundStyle(.black)
        .disabled(link.status != .connected)
    }

    @ViewBuilder private var cards: some View {
        let approvals = app.today?.array("approvals") ?? []
        if !approvals.isEmpty {
            section("Needs your OK")
            ForEach(Array(approvals.enumerated()), id: \.offset) { _, a in
                VStack(alignment: .leading, spacing: 8) {
                    Text(a.string("what") ?? "").font(.subheadline.weight(.semibold))
                    Text(a.string("command") ?? "").font(.footnote.monospaced()).foregroundStyle(.secondary)
                    HStack {
                        Button("Allow") { decide(a, true) }.buttonStyle(.borderedProminent).tint(.green)
                        Button("Deny") { decide(a, false) }.buttonStyle(.bordered)
                    }
                }
                .padding(12).frame(maxWidth: .infinity, alignment: .leading)
                .background(.white.opacity(0.06), in: RoundedRectangle(cornerRadius: 12))
            }
        }
    }

    private func decide(_ approval: JSON, _ allow: Bool) {
        guard let id = approval.int("id") else { return }
        let launch = app.today?.string("launch") ?? ""
        Task { try? await link.decide(approval: id, allow: allow, launch: launch); await app.refresh() }
    }

    @ViewBuilder private var sessionsList: some View {
        if !app.sessions.isEmpty {
            section("Sessions")
            ForEach(Array(app.sessions.prefix(12).enumerated()), id: \.offset) { _, entry in
                if let session = entry.object("session"), let id = session.string("id") {
                    Button { openSession = id } label: {
                        HStack {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(session.string("title") ?? "Session").font(.subheadline)
                                Text(session.string("branch") ?? "").font(.caption.monospaced()).foregroundStyle(.secondary)
                            }
                            Spacer()
                            if entry.object("prompt") != nil { Text("Asks").font(.caption).foregroundStyle(.orange) }
                            else if entry.string("activity") == "working" { Text("Working").font(.caption).foregroundStyle(.secondary) }
                        }
                    }
                    .buttonStyle(.plain)
                    .padding(.vertical, 4)
                }
            }
        }
    }

    @ViewBuilder private var conversationList: some View {
        if !app.conversation.isEmpty {
            section("Conversation")
            ForEach(Array(app.conversation.suffix(20).enumerated()), id: \.offset) { _, item in
                let mine = (item["author"] as? String) == "person"
                Text(item.string("text") ?? "")
                    .font(.callout)
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: mine ? .trailing : .leading)
                    .background((mine ? gold.opacity(0.18) : Color.white.opacity(0.06)), in: RoundedRectangle(cornerRadius: 10))
            }
        }
    }

    private func section(_ title: String) -> some View {
        Text(title.uppercased()).font(.caption.weight(.semibold)).foregroundStyle(.secondary).padding(.top, 6)
    }
}

/// One session: its last reply, its question with the options a phone may pick, and its
/// terminal, view only.
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
                    Text(card.state).font(.footnote).foregroundStyle(.secondary)
                    if let question = card.question {
                        Text("Asks: \(question)").font(.headline)
                    }
                    if !card.lastReply.isEmpty { Text(card.lastReply).font(.callout) }
                }
                HStack {
                    TextField("Say something to this session", text: $message).textFieldStyle(.roundedBorder)
                    Button("Send") {
                        let text = message
                        message = ""
                        Task { try? await link.send(session: id, text: text) }
                    }.disabled(message.isEmpty)
                }
                Text("TERMINAL · VIEW ONLY").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                Text(screen.isEmpty ? "…" : screen).font(.caption2.monospaced()).frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10).background(.white.opacity(0.05), in: RoundedRectangle(cornerRadius: 8))
            }
            .padding(16)
        }
        .navigationTitle(card?.title ?? "Session")
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
