import SwiftUI
import MWDATCore

// Tracks DAT registration so the UI can show Connect / Registered.
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
    func unregister() { Task { try? await Wearables.shared.startUnregistration() } }
}

struct ContentView: View {
    @StateObject private var reg = Registration()
    @StateObject private var glasses: GlassesSession
    @StateObject private var glasscode: GlasscodeVoice
    @StateObject private var flow: GlasscodeFlow
    @Environment(\.scenePhase) private var scenePhase
    @State private var showAdvanced = false

    init() {
        let g = GlassesSession()
        let l = GlasscodeVoice()
        _glasses = StateObject(wrappedValue: g)
        _glasscode = StateObject(wrappedValue: l)
        _flow = StateObject(wrappedValue: GlasscodeFlow(glasses: g, glasscode: l))
    }

    private var gold: Color { Color(red: 0.86, green: 0.70, blue: 0.36) }

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            VStack(spacing: 16) {
                header
                mirror
                    .frame(maxHeight: .infinity)
                navPad
                controls
            }
            .padding(20)
        }
        .preferredColorScheme(.dark)
        .onAppear {
            flow.start()
        }
        .onChange(of: scenePhase) { phase in
            guard phase == .active else { return }
            // Returning from Meta AI (e.g. after granting the mic) can drop the session —
            // reconnect so the user never has to tap Connect again.
            if reg.state == .registered, !glasses.isConnected { glasses.connect() }
            // Coming back from background (often after a network change or the Mac sleeping):
            // re-probe the control plane and recover if we were stuck.
            Task { await flow.recover() }
        }
    }

    // MARK: header

    private var header: some View {
        HStack(alignment: .top) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Glasscode").font(.largeTitle.bold()).foregroundStyle(gold)
                Text(glasses.isConnected ? "On your glasses" : glasses.statusText)
                    .font(.caption).foregroundStyle(glasses.isConnected ? .green : .secondary)
            }
            Spacer()
            costChip
            Button { showAdvanced.toggle() } label: {
                Image(systemName: "ellipsis.circle").font(.title3).foregroundStyle(.secondary)
            }
            .focusable(false)
        }
    }

    @ViewBuilder private var costChip: some View {
        HStack(spacing: 7) {
            Circle().fill(flow.billing ? .orange : .green).frame(width: 8, height: 8)
            Text(flow.billing ? flow.costText : "free")
                .font(.system(.footnote, design: .monospaced))
                .foregroundStyle(flow.billing ? .orange : .green)
        }
        .padding(.horizontal, 12).padding(.vertical, 7)
        .background(.white.opacity(0.06), in: Capsule())
    }

    // MARK: the glasses mirror

    private var mirror: some View {
        VStack(alignment: .leading, spacing: 12) {
            // Crumb stays pinned above the scroll area so the header is never lost.
            Text(flow.crumb.uppercased())
                .font(.caption2.weight(.semibold)).tracking(1)
                .foregroundStyle(.secondary)

            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        mirrorBody
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                .scrollIndicators(.visible)
                // Keep the band-selected row in view as you swipe through a long list.
                .onChange(of: selectedRowId) { _, id in
                    guard let id else { return }
                    withAnimation(.easeOut(duration: 0.15)) { proxy.scrollTo(id, anchor: .center) }
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
        .padding(16)
        .background(.white.opacity(0.05), in: RoundedRectangle(cornerRadius: 18))
    }

    @ViewBuilder private var mirrorBody: some View {
        switch flow.screen {
        case .voice:  livePane(title: flow.listening ? "● Listening — free" : "Voice",
                               body: flow.detail.isEmpty ? "Pinch ● to speak. Your words go to the thread." : flow.detail,
                               tint: .green)
        case .talk:
            livePane(title: "Live with Glasscode",
                     body: flow.banner.isEmpty ? "Ask about this session, or say “jump to…”." : flow.banner,
                     tint: .orange)
            Button(role: .destructive) { flow.handle(.back) } label: {
                Label("Hang up", systemImage: "phone.down.fill")
                    .frame(maxWidth: .infinity).padding(.vertical, 12)
                    .background(.orange.opacity(0.18), in: RoundedRectangle(cornerRadius: 12))
                    .foregroundStyle(.orange)
            }
            .focusable(false)
        case .loading: placeholder("Loading your projects…")
        case .error:   placeholder("Can't reach Glasscode — reconnecting automatically…")
        default:
            if !flow.banner.isEmpty { bannerLine(flow.banner) }
            VStack(spacing: 8) {
                ForEach(flow.rows) { row in rowView(row).id(row.id) }
            }
        }
    }

    private var selectedRowId: String? { flow.rows.first(where: { $0.selected })?.id }

    private func rowView(_ row: GlasscodeRow) -> some View {
        HStack(spacing: 12) {
            Circle().fill(row.selected ? gold : .white.opacity(0.18)).frame(width: 8, height: 8)
            VStack(alignment: .leading, spacing: 2) {
                Text(row.title).font(.body.weight(row.selected ? .semibold : .regular)).foregroundStyle(.white)
                if let s = row.subtitle, !s.isEmpty {
                    Text(s).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
            }
            Spacer()
        }
        .padding(.horizontal, 12).padding(.vertical, 10)
        .background(row.selected ? gold.opacity(0.14) : .clear, in: RoundedRectangle(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).stroke(row.selected ? gold.opacity(0.5) : .clear, lineWidth: 1))
        .contentShape(Rectangle())
        .onTapGesture { flow.tapRow(row) }
    }

    private func livePane(title: String, body: String, tint: Color) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.headline).foregroundStyle(tint)
            Text(body).font(.subheadline).foregroundStyle(.white).fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(14)
        .background(tint.opacity(0.1), in: RoundedRectangle(cornerRadius: 14))
    }

    private func placeholder(_ text: String) -> some View {
        Text(text).font(.subheadline).foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 10)
    }

    private func bannerLine(_ text: String) -> some View {
        Text(text).font(.footnote).foregroundStyle(gold)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: nav pad (neural-band fallback + mirror)

    private var navPad: some View {
        VStack(spacing: 8) {
            Text(bandStatus)
                .font(.caption2).foregroundStyle(glasses.inputsAvailable ? .green : .secondary)
                .multilineTextAlignment(.center)
            HStack(spacing: 10) {
                padButton("chevron.up") { flow.handle(.up) }
                padButton("chevron.down") { flow.handle(.down) }
                padButton("arrow.uturn.left") { flow.handle(.back) }
                Button { flow.handle(.select) } label: {
                    Text(selectLabel).font(.body.weight(.semibold))
                        .frame(maxWidth: .infinity).padding(.vertical, 14)
                        .background(gold, in: RoundedRectangle(cornerRadius: 12))
                        .foregroundStyle(.black)
                }
            }
            if showAdvanced {
                Text(glasses.lastEvent.isEmpty ? "last band event: —" : "last: \(glasses.lastEvent)")
                    .font(.caption2.monospaced()).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading).lineLimit(1)
            }
        }
    }

    private var selectLabel: String {
        switch flow.screen {
        case .voice: return flow.listening ? "Send" : "Speak"
        case .actions: return "Start"
        case .talk: return "—"
        default: return "Open"
        }
    }

    private var bandStatus: String {
        guard glasses.isConnected else { return "Drive Glasscode from here" }
        if glasses.inputsAvailable { return "Neural band connected · look at the lens" }
        if let e = glasses.inputsError { return "Band not attached — \(e)\nUse these buttons" }
        return "Band not attached · use these buttons"
    }

    private func padButton(_ icon: String, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: icon).font(.body.weight(.semibold))
                .frame(width: 52, height: 48)
                .background(.white.opacity(0.08), in: RoundedRectangle(cornerRadius: 12))
                .foregroundStyle(.white)
        }
    }

    // MARK: connection controls

    @ViewBuilder private var controls: some View {
        VStack(spacing: 10) {
            if glasses.requiresDATAppUpdate {
                Button("Update glasses app") { Task { try? await Wearables.shared.openDATGlassesAppUpdate() } }
                    .buttonStyle(.borderedProminent)
            }
            switch reg.state {
            case .registered:
                if glasses.isConnected {
                    Label("Glasses connected", systemImage: "checkmark.circle.fill")
                        .font(.footnote).foregroundStyle(.green)
                } else {
                    Button("Connect glasses") { glasses.connect() }
                        .buttonStyle(.bordered).tint(gold).focusable(false)
                }
                // Destructive teardown lives behind the ⋯ menu + a confirm, so a stray
                // neural-band click can never disconnect Meta AI by accident.
                if showAdvanced {
                    Button(role: .destructive) { reg.unregister(); showAdvanced = false } label: {
                        Text("Disconnect Meta AI")
                    }
                    .font(.footnote).focusable(false)
                }
            case .registering:
                ProgressView().tint(gold)
                Text("Approve in the Meta AI app…").font(.footnote).foregroundStyle(.secondary)
            default:
                Button("Connect to Meta AI") { reg.register() }
                    .buttonStyle(.borderedProminent).tint(gold).focusable(false)
                Text("Enable Developer Mode in Meta AI › Your glasses first.")
                    .font(.caption2).foregroundStyle(.secondary).multilineTextAlignment(.center)
            }
        }
    }
}
