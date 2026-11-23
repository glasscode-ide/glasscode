import SwiftUI

/// The licences this app carries, as its licences ask: Glasscode's own notice and licence
/// (Apache-2.0, the repository's NOTICE and LICENSE) and those of the packages it is built with
/// (Licenses/ThirdPartyNotices.txt, written by script/swift-notices.py).
struct LicencesView: View {
    private struct Document: Identifiable {
        let title: String
        let resource: String
        let ext: String?
        var id: String { resource }
    }

    private static let documents = [
        Document(title: "Glasscode: notice", resource: "NOTICE", ext: nil),
        Document(title: "Glasscode: licence (Apache-2.0)", resource: "LICENSE", ext: nil),
        Document(title: "Third-party packages", resource: "ThirdPartyNotices", ext: "txt"),
    ]

    var body: some View {
        List(Self.documents) { document in
            NavigationLink(document.title) {
                LicenceText(title: document.title, text: Self.text(of: document))
            }
        }
        .navigationTitle("Licences")
    }

    private static func text(of document: Document) -> String {
        guard let url = Bundle.main.url(forResource: document.resource, withExtension: document.ext),
              let text = try? String(contentsOf: url, encoding: .utf8)
        else { return "This build does not carry \(document.resource)." }
        return text
    }
}

/// A long licence text, a paragraph at a time so that only what is on screen is laid out.
private struct LicenceText: View {
    let title: String
    let paragraphs: [String]

    init(title: String, text: String) {
        self.title = title
        paragraphs = text.components(separatedBy: "\n\n").filter { !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
    }

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 10) {
                ForEach(paragraphs.indices, id: \.self) { index in
                    Text(paragraphs[index])
                        .font(.system(.footnote, design: .monospaced))
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            .textSelection(.enabled)
            .padding()
        }
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.inline)
    }
}
