import PhotosUI
import SwiftUI
import UIKit

struct ContentView: View {
    @StateObject private var session = AppSession()
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        Group {
            switch session.stage {
            case .connecting:
                ProgressScreen(message: "Connecting to Bogkit…")
            case .serverSelection:
                ServerSelectionView(session: session)
            case .registration:
                RegistrationView(session: session)
            case .hub:
                HubView(session: session)
            }
        }
        .preferredColorScheme(.dark)
        .task { session.setForeground(scenePhase == .active) }
        .onReceive(session.camera.$calibrationState) { session.syncCalibration($0) }
        .onReceive(session.game.$match) { match in
            guard let match else { return }
            switch match.status {
            case .lobby, .briefing: session.cover = .lobby
            case .active: session.cover = .gameplay
            case .completed: session.cover = .result
            }
        }
        .onChange(of: scenePhase) { _, phase in
            session.setForeground(phase == .active)
            guard phase == .active, session.stage == .hub else { return }
            Task {
                await session.refreshFriends()
                await session.loadHistory()
            }
        }
        .fullScreenCover(item: $session.cover) { cover in
            switch cover {
            case .appearance:
                AppearanceEnrollmentView(session: session)
            case .calibration:
                CalibrationFlowView(camera: session.camera)
            case .lobby:
                MatchLobbyView(session: session)
            case .gameplay:
                GameplayCameraView(camera: session.camera, game: session.game)
            case .result:
                MatchResultView(session: session)
            }
        }
    }
}

private struct ServerSelectionView: View {
    @ObservedObject var session: AppSession
    @State private var customAddress = ""
    @State private var showAdvanced = false

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    VStack(alignment: .leading, spacing: 8) {
                        Image(systemName: "scope")
                            .font(.system(size: 44, weight: .black))
                            .foregroundStyle(.red)
                        Text("UNTITLED FPS")
                            .font(.largeTitle.weight(.black).monospaced())
                        Text("Connect to a Bogkit server to register, calibrate, and enter a match.")
                            .foregroundStyle(.secondary)
                    }

                    if let server = session.defaultServer {
                        serverButton(server, label: "DEFAULT SERVER", icon: "bolt.horizontal.circle.fill")
                    } else {
                        SetupCard {
                            Label("Default server unavailable", systemImage: "server.rack")
                            Text("This build has no FPS_DEFAULT_SERVER_URL. Use a custom server below.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }

                    DisclosureGroup("Custom / recent servers", isExpanded: $showAdvanced) {
                        VStack(spacing: 12) {
                            TextField("http://192.168.1.4:3000", text: $customAddress)
                                .textInputAutocapitalization(.never)
                                .keyboardType(.URL)
                                .textFieldStyle(.roundedBorder)
                            Button {
                                guard let endpoint = ServerEndpoint.parse(customAddress) else { return }
                                Task { await session.connect(to: endpoint) }
                            } label: {
                                Label("Connect custom server", systemImage: "network")
                                    .frame(maxWidth: .infinity)
                            }
                            .buttonStyle(.borderedProminent)
                            .disabled(ServerEndpoint.parse(customAddress) == nil)

                            ForEach(session.recentServers) { server in
                                serverButton(server, label: server.displayName.uppercased(), icon: "clock.arrow.circlepath")
                            }
                        }
                        .padding(.top, 12)
                    }

                    if let message = session.message {
                        Label(message, systemImage: "exclamationmark.triangle.fill")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                }
                .padding(24)
            }
            .background(AppBackground())
            .navigationTitle("Connect")
            .navigationBarTitleDisplayMode(.inline)
        }
        .onAppear { session.game.requestLocalNetworkAccess() }
    }

    private func serverButton(_ server: ServerEndpoint, label: String, icon: String) -> some View {
        Button {
            Task { await session.connect(to: server) }
        } label: {
            SetupCard {
                HStack {
                    Image(systemName: icon).foregroundStyle(.green)
                    VStack(alignment: .leading, spacing: 4) {
                        Text(label).font(.headline.monospaced())
                        Text(server.canonicalAddress).font(.caption.monospaced()).foregroundStyle(.secondary)
                    }
                    Spacer()
                    Image(systemName: "chevron.right")
                }
            }
        }
        .buttonStyle(.plain)
        .disabled(session.busy)
    }
}

private struct RegistrationView: View {
    @ObservedObject var session: AppSession
    @State private var handle = ""
    @State private var displayName = ""

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Text("Your account belongs to \(session.activeServer?.displayName ?? "this server").")
                        .foregroundStyle(.secondary)
                }
                Section("PLAYER IDENTITY") {
                    TextField("Unique handle", text: $handle)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    TextField("Display name", text: $displayName)
                    Text("Friends find you by an exact handle. Your display name is what they see in matches.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Section {
                    Button {
                        Task { await session.register(handle: normalizedHandle, displayName: displayName.trimmed) }
                    } label: {
                        if session.busy {
                            ProgressView().frame(maxWidth: .infinity)
                        } else {
                            Text("Create player").frame(maxWidth: .infinity)
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(!valid || session.busy)
                }
                if let message = session.message {
                    Section("STATUS") { Text(message).foregroundStyle(.orange) }
                }
            }
            .navigationTitle("Register")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Back") { session.switchServer() }
                        .disabled(session.busy)
                }
            }
        }
    }

    private var normalizedHandle: String {
        handle.trimmed.lowercased()
    }

    private var valid: Bool {
        (3...20).contains(normalizedHandle.count)
            && normalizedHandle.allSatisfy { $0.isLetter || $0.isNumber || $0 == "-" || $0 == "_" }
            && (2...32).contains(displayName.trimmed.count)
    }
}

private struct HubView: View {
    @ObservedObject var session: AppSession

    var body: some View {
        TabView(selection: $session.selectedTab) {
            NavigationStack { PlayView(session: session) }
                .tabItem { Label("Play", systemImage: "scope") }
                .tag(HubTab.play)
            NavigationStack { FriendsView(session: session) }
                .tabItem { Label("Friends", systemImage: "person.2.fill") }
                .tag(HubTab.friends)
            NavigationStack { HistoryView(session: session) }
                .tabItem { Label("History", systemImage: "clock.fill") }
                .tag(HubTab.history)
            NavigationStack { ProfileView(session: session) }
                .tabItem { Label("Profile", systemImage: "person.crop.circle.fill") }
                .tag(HubTab.profile)
        }
        .tint(.red)
    }
}

private struct PlayView: View {
    @ObservedObject var session: AppSession
    @State private var inviteCode = ""

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                VStack(alignment: .leading, spacing: 3) {
                    Text("READY CHECK").font(.caption.bold().monospaced()).foregroundStyle(.green)
                    Text("Hey, \(session.account?.displayName ?? "player").")
                        .font(.title.bold())
                    Text(session.activeServer?.displayName ?? "")
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                }

                SetupCard {
                    ForEach(SetupRequirement.allCases, id: \.self) { requirement in
                        let missing = session.readiness.missingRequirements.contains(requirement)
                        Button {
                            if missing { session.open(requirement) }
                        } label: {
                            HStack {
                                Image(systemName: missing ? "circle" : "checkmark.circle.fill")
                                    .foregroundStyle(missing ? .orange : .green)
                                Text(requirement.title)
                                Spacer()
                                if missing { Image(systemName: "chevron.right") }
                            }
                        }
                        .buttonStyle(.plain)
                        if requirement != SetupRequirement.allCases.last { Divider() }
                    }
                }

                if let invitations = session.invitationsState.value, !invitations.isEmpty {
                    Text("CHALLENGES").font(.caption.bold().monospaced()).foregroundStyle(.orange)
                    ForEach(invitations) { invitation in
                        SetupCard {
                            Text("Friend challenge").font(.headline)
                            Text("Expires \(Date(timeIntervalSince1970: Double(invitation.expiresAtMs) / 1_000), style: .relative)")
                                .font(.caption).foregroundStyle(.secondary)
                            HStack {
                                Button("Accept") { Task { await session.resolve(invitation, accept: true) } }
                                    .buttonStyle(.borderedProminent).tint(.green)
                                    .disabled(!session.readiness.canEnterMatch)
                                Button("Decline", role: .destructive) {
                                    Task { await session.resolve(invitation, accept: false) }
                                }
                                .buttonStyle(.bordered)
                            }
                        }
                    }
                }

                Text("MATCH ENTRY").font(.caption.bold().monospaced()).foregroundStyle(.secondary)
                actionButton("Challenge friend", icon: "person.crop.circle.badge.plus") {
                    Task { await session.refreshFriends() }
                } destination: {
                    FriendChallengeView(session: session)
                }
                Button {
                    Task { await session.game.createInvite() }
                } label: {
                    MatchActionLabel(title: "Create code", icon: "square.and.arrow.up")
                }
                .buttonStyle(.plain)
                .disabled(!session.readiness.canEnterMatch || session.game.busy)

                Button {
                    Task { await session.game.quickMatchNearby() }
                } label: {
                    MatchActionLabel(title: "Quick match nearby", icon: "location.magnifyingglass")
                }
                .buttonStyle(.plain)
                .disabled(!session.readiness.canEnterMatch || session.game.busy)

                SetupCard {
                    TextField("Invite code", text: $inviteCode)
                        .textInputAutocapitalization(.characters)
                        .autocorrectionDisabled()
                    Button("Join code") {
                        Task { await session.game.joinInvite(code: inviteCode.trimmed) }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(!session.readiness.canEnterMatch || inviteCode.trimmed.isEmpty)
                }

#if DEBUG
                Button {
                    Task { await session.game.createInvite() }
                } label: {
                    MatchActionLabel(title: "Solo test", icon: "testtube.2")
                }
                .buttonStyle(.plain)
                .disabled(!session.readiness.canEnterMatch)

                DiagnosticRecordingCard(camera: session.camera)
#endif

                if let message = session.message ?? Optional(session.game.statusMessage) {
                    Text(message).font(.caption).foregroundStyle(.secondary)
                }
            }
            .padding(18)
        }
        .background(AppBackground())
        .navigationTitle("Play")
    }

    private func actionButton<Destination: View>(
        _ title: String,
        icon: String,
        action: @escaping () -> Void,
        @ViewBuilder destination: () -> Destination
    ) -> some View {
        NavigationLink(destination: destination()) {
            MatchActionLabel(title: title, icon: icon)
        }
        .simultaneousGesture(TapGesture().onEnded(action))
        .buttonStyle(.plain)
        .disabled(!session.readiness.canEnterMatch)
    }
}

private struct MatchActionLabel: View {
    let title: String
    let icon: String

    var body: some View {
        SetupCard {
            HStack {
                Image(systemName: icon).font(.title2).foregroundStyle(.red)
                Text(title).font(.headline)
                Spacer()
                Image(systemName: "chevron.right")
            }
        }
    }
}

private struct AppearanceEnrollmentView: View {
    @ObservedObject var session: AppSession
    @Environment(\.dismiss) private var dismiss
    @State private var bodyImage: UIImage?
    @State private var faceImage: UIImage?
    @State private var selectedBody: PhotosPickerItem?
    @State private var selectedFace: PhotosPickerItem?
    @State private var capture: CaptureKind?

    private enum CaptureKind: String, Identifiable {
        case body
        case face
        var id: String { rawValue }
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 18) {
                    enrollmentCard(
                        number: "01",
                        title: "Full-body outfit",
                        instructions: "Stand in even light with your whole outfit visible. Your description is generated from this photo and cannot be edited.",
                        image: bodyImage,
                        capture: .body,
                        selection: $selectedBody
                    )
                    enrollmentCard(
                        number: "02",
                        title: "Face briefing",
                        instructions: "Face the camera without covering your face. This stays match-scoped and creates your briefing thumbnail.",
                        image: faceImage,
                        capture: .face,
                        selection: $selectedFace
                    )
                    // Chosen before the upload rather than after it, so the
                    // first profile a player publishes already carries their
                    // skin instead of silently defaulting.
                    SetupCard {
                        HStack(alignment: .top) {
                            Text("03").font(.headline.monospaced()).foregroundStyle(.red)
                            VStack(alignment: .leading, spacing: 10) {
                                Text("Silhouette skin").font(.headline)
                                SilhouetteSkinPicker(session: session)
                            }
                        }
                    }
                    Button {
                        guard let bodyImage, let faceImage else { return }
                        Task {
                            if await session.enrollAppearance(bodyImage: bodyImage, faceImage: faceImage) {
                                self.bodyImage = nil
                                self.faceImage = nil
                                self.selectedBody = nil
                                self.selectedFace = nil
                                dismiss()
                            }
                        }
                    } label: {
                        if session.busy {
                            ProgressView().frame(maxWidth: .infinity)
                        } else {
                            Text("Generate appearance").frame(maxWidth: .infinity)
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(.red)
                    .disabled(bodyImage == nil || faceImage == nil || session.busy)
                    if let message = session.message {
                        Text(message).font(.caption).foregroundStyle(.secondary)
                    }
                }
                .padding(18)
            }
            .background(AppBackground())
            .navigationTitle("Appearance")
            .toolbar { Button("Close") { dismiss() } }
            .sheet(item: $capture) { kind in
                EnrollmentPhotoPicker(cameraDevice: kind == .face ? .front : .rear) { image in
                    if kind == .body { bodyImage = image } else { faceImage = image }
                }
                .ignoresSafeArea()
            }
            .onChange(of: selectedBody) { _, item in loadBody(item) }
            .onChange(of: selectedFace) { _, item in loadFace(item) }
        }
    }

    private func enrollmentCard(
        number: String,
        title: String,
        instructions: String,
        image: UIImage?,
        capture kind: CaptureKind,
        selection: Binding<PhotosPickerItem?>
    ) -> some View {
        SetupCard {
            HStack(alignment: .top) {
                Text(number).font(.headline.monospaced()).foregroundStyle(.red)
                VStack(alignment: .leading, spacing: 10) {
                    Text(title).font(.headline)
                    Text(instructions).font(.caption).foregroundStyle(.secondary)
                    if let image {
                        Image(uiImage: image)
                            .resizable()
                            .scaledToFill()
                            .frame(height: 180)
                            .clipShape(RoundedRectangle(cornerRadius: 10))
                    }
                    HStack {
                        Button("Take photo") { capture = kind }.buttonStyle(.bordered)
                        PhotosPicker(selection: selection, matching: .images) {
                            Text("Choose test photo")
                        }
                        .buttonStyle(.bordered)
                    }
                }
            }
        }
    }

    private func loadBody(_ item: PhotosPickerItem?) {
        Task {
            guard let data = try? await item?.loadTransferable(type: Data.self),
                  let image = UIImage(data: data) else { return }
            bodyImage = image
        }
    }

    private func loadFace(_ item: PhotosPickerItem?) {
        Task {
            guard let data = try? await item?.loadTransferable(type: Data.self),
                  let image = UIImage(data: data) else { return }
            faceImage = image
        }
    }
}

private struct CalibrationFlowView: View {
    @ObservedObject var camera: CameraService
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            CameraPreview(session: camera.session).ignoresSafeArea()
            if let target = camera.currentCalibrationTarget {
                CalibrationTarget(target: target)
            }
            VStack {
                HStack {
                    Button("Close") { dismiss() }
                        .buttonStyle(.borderedProminent)
                        .tint(.black.opacity(0.72))
                    Spacer()
                }
                Spacer()
                VStack(spacing: 10) {
                    Text("FINGER-GUN CALIBRATION").font(.headline.monospaced())
                    Text(calibrationMessage).multilineTextAlignment(.center)
                    if case .collecting(let progress, _) = camera.calibrationState {
                        ProgressView(value: progress).tint(.green)
                    }
                    if case .calibrated = camera.calibrationState {
                        Button("Done") { dismiss() }.buttonStyle(.borderedProminent).tint(.green)
                    } else {
                        Button("Start five points") { camera.beginCalibration() }
                            .buttonStyle(.borderedProminent)
                    }
                }
                .padding()
                .background(.black.opacity(0.78), in: RoundedRectangle(cornerRadius: 14))
            }
            .foregroundStyle(.white)
            .padding()
        }
        .onAppear { camera.start() }
        .onDisappear {
            camera.finalizeRecording()
            camera.stop()
        }
        .statusBarHidden()
    }

    private var calibrationMessage: String {
        switch camera.calibrationState {
        case .required: "Use a natural thumb-up finger gun. Hold center, left, right, up, and down as prompted."
        case .collecting: camera.calibrationInstruction ?? "Hold steady."
        case .failed(let message): message
        case .calibrated: "Calibration saved for this camera and model."
        }
    }
}

private struct CalibrationTarget: View {
    let target: VisionCalibrationTarget

    var body: some View {
        GeometryReader { proxy in
            let point = CGPoint(x: target.point.x * proxy.size.width, y: (1 - target.point.y) * proxy.size.height)
            ZStack {
                Circle().stroke(.white, lineWidth: 3).frame(width: 50, height: 50)
                Circle().fill(.red).frame(width: 8, height: 8)
                Rectangle().fill(.white).frame(width: 68, height: 2)
                Rectangle().fill(.white).frame(width: 2, height: 68)
            }
            .position(point)
        }
        .allowsHitTesting(false)
    }
}

private struct FriendChallengeView: View {
    @ObservedObject var session: AppSession
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        LoadStateList(state: session.friendsState, empty: "Add a friend before challenging them.") { friends in
            ForEach(friends) { friend in
                Button {
                    Task {
                        await session.game.challenge(friend)
                        if session.game.match != nil { dismiss() }
                    }
                } label: {
                    FriendRow(friend: friend, trailing: "Challenge")
                }
            }
        }
        .navigationTitle("Challenge")
        .task { await session.refreshFriends() }
    }
}

private struct FriendsView: View {
    @ObservedObject var session: AppSession
    @State private var handle = ""

    var body: some View {
        List {
            Section("FIND BY EXACT HANDLE") {
                HStack {
                    TextField("handle", text: $handle)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    Button("Search") { Task { await session.searchPlayer(handle: handle.trimmed) } }
                        .disabled(handle.trimmed.isEmpty)
                }
                searchResult
            }

            if let requests = session.requestsState.value, !requests.isEmpty {
                Section("REQUESTS") {
                    ForEach(requests) { request in
                        VStack(alignment: .leading, spacing: 8) {
                            FriendRow(friend: request.sender, trailing: nil)
                            HStack {
                                Button("Accept") { Task { await session.resolve(request, accept: true) } }
                                    .buttonStyle(.borderedProminent).tint(.green)
                                Button("Decline", role: .destructive) {
                                    Task { await session.resolve(request, accept: false) }
                                }
                                .buttonStyle(.bordered)
                            }
                        }
                    }
                }
            }

            Section("FRIENDS") {
                switch session.friendsState {
                case .idle, .loading:
                    ProgressView()
                case .failed(let message):
                    ContentUnavailableView("Friends unavailable", systemImage: "wifi.exclamationmark", description: Text(message))
                case .loaded(let friends):
                    if friends.isEmpty {
                        ContentUnavailableView("No friends yet", systemImage: "person.2")
                    } else {
                        ForEach(friends) { friend in
                            FriendRow(friend: friend, trailing: nil)
                                .swipeActions {
                                    Button("Remove", role: .destructive) { Task { await session.remove(friend) } }
                                }
                        }
                    }
                }
            }
        }
        .navigationTitle("Friends")
        .refreshable { await session.refreshFriends() }
        .task { if session.friendsState.value == nil { await session.refreshFriends() } }
    }

    @ViewBuilder private var searchResult: some View {
        switch session.playerSearchState {
        case .idle:
            EmptyView()
        case .loading:
            ProgressView()
        case .failed(let message):
            Text(message).font(.caption).foregroundStyle(.orange)
        case .loaded(.none):
            Text("No exact handle match.").foregroundStyle(.secondary)
        case .loaded(.some(let player)):
            HStack {
                VStack(alignment: .leading) {
                    Text(player.displayName)
                    Text("@\(player.handle)").font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                Button("Add") { Task { await session.sendFriendRequest(to: player) } }
                    .buttonStyle(.borderedProminent)
            }
        }
    }
}

private struct FriendRow: View {
    let friend: FriendSummary
    let trailing: String?

    var body: some View {
        HStack {
            Circle()
                .fill(friend.available ? .green : .gray)
                .frame(width: 9, height: 9)
            VStack(alignment: .leading) {
                Text(friend.displayName)
                Text("@\(friend.handle)").font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
            if let trailing { Text(trailing).font(.caption.bold()).foregroundStyle(.red) }
        }
        .contentShape(Rectangle())
    }
}

private struct HistoryView: View {
    @ObservedObject var session: AppSession

    var body: some View {
        LoadStateList(state: session.historyState, empty: "Completed matches will appear here.") { matches in
            ForEach(matches) { match in
                NavigationLink {
                    MatchHistoryDetailView(session: session, matchId: match.matchId)
                } label: {
                    VStack(alignment: .leading, spacing: 5) {
                        HStack {
                            Text(match.result.rawValue.uppercased())
                                .font(.caption.bold().monospaced())
                                .foregroundStyle(match.result == .won ? .green : .red)
                            Spacer()
                            Text(Date(timeIntervalSince1970: Double(match.completedAtMs) / 1_000), style: .relative)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        Text(match.opponent.displayName).font(.headline)
                        Text("\(match.durationSeconds)s · \(match.myHitTotal) hits")
                            .font(.caption.monospaced())
                            .foregroundStyle(.secondary)
                    }
                }
            }
            if session.canLoadMoreHistory {
                Button("Load more") { Task { await session.loadHistory(reset: false) } }
                    .frame(maxWidth: .infinity)
            }
        }
        .navigationTitle("History")
        .refreshable { await session.loadHistory() }
        .task { if session.historyState.value == nil { await session.loadHistory() } }
    }
}

private struct MatchHistoryDetailView: View {
    @ObservedObject var session: AppSession
    let matchId: String

    var body: some View {
        Group {
            switch session.selectedHistoryState {
            case .idle, .loading:
                ProgressView()
            case .failed(let message):
                ContentUnavailableView("Match unavailable", systemImage: "exclamationmark.triangle", description: Text(message))
            case .loaded(let detail):
                List {
                    Section("PARTICIPANTS") {
                        ForEach(detail.participants) { participant in
                            LabeledContent(
                                participant.displayName,
                                value: "\(participant.hitTotal) hits\(participant.winner ? " · winner" : "")"
                            )
                        }
                    }
                    Section("TIMELINE") {
                        ForEach(detail.events) { event in
                            VStack(alignment: .leading) {
                                Text(event.type.replacingOccurrences(of: "_", with: " ").uppercased())
                                    .font(.caption.bold().monospaced())
                                if let detail = event.detail { Text(detail).foregroundStyle(.secondary) }
                                Text(Date(timeIntervalSince1970: Double(event.timestampMs) / 1_000), style: .time)
                                    .font(.caption2).foregroundStyle(.secondary)
                            }
                        }
                    }
                }
            }
        }
        .navigationTitle("Match")
        .task { await session.loadHistoryDetail(matchId) }
    }
}

private struct ProfileView: View {
    @ObservedObject var session: AppSession
    @State private var confirmSwitch = false

    private static func thumbnail(_ base64: String?) -> Image? {
        guard let base64,
              let data = Data(base64Encoded: base64),
              let uiImage = UIImage(data: data) else { return nil }
        return Image(uiImage: uiImage)
    }

    var body: some View {
        List {
            Section("PLAYER") {
                LabeledContent("Display name", value: session.account?.displayName ?? "—")
                LabeledContent("Handle", value: session.account.map { "@\($0.handle)" } ?? "—")
            }
            Section("SERVER") {
                LabeledContent("Name", value: session.activeServer?.displayName ?? "—")
                LabeledContent("Environment", value: session.serverInfo?.environment ?? "—")
                Text(session.activeServer?.canonicalAddress ?? "")
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                Button("Switch server", role: .destructive) {
                    if session.canSwitchServerImmediately { session.switchServer() } else { confirmSwitch = true }
                }
            }
            Section("APPEARANCE") {
                if let profile = session.appearanceProfile {
                    if let image = Self.thumbnail(profile.briefingThumbnail) {
                        image
                            .resizable()
                            .scaledToFill()
                            .frame(width: 96, height: 96)
                            .clipShape(RoundedRectangle(cornerRadius: 12))
                    }
                    Text(profile.generatedDescription)
                    Label("Body + face enrolled", systemImage: "checkmark.seal.fill").foregroundStyle(.green)
                    Button("Update appearance") { session.cover = .appearance }
                } else {
                    Button("Set up appearance") { session.cover = .appearance }
                }
                LabeledContent(
                    "Recognition model",
                    value: MobileCLIPEmbedder.shared.isAvailable ? "MobileCLIP2-S0" : "Fallback (color grid)"
                )
            }
            Section("SILHOUETTE SKIN") {
                SilhouetteSkinPicker(session: session)
            }
#if DEBUG
            DiagnosticRecordingListSection(camera: session.camera)
            Section("DEVELOPER") {
                Button("Reset calibration", role: .destructive) {
                    session.camera.resetCalibration()
                }
            }
#endif
        }
        .navigationTitle("Profile")
        .alert("Leave current match?", isPresented: $confirmSwitch) {
            Button("Cancel", role: .cancel) {}
            Button("Leave and switch", role: .destructive) { session.switchServer() }
        } message: {
            Text("Switching servers disconnects the current lobby or match.")
        }
    }
}

#if DEBUG
private struct DiagnosticRecordingCard: View {
    @ObservedObject var camera: CameraService

    var body: some View {
        if let url = camera.lastRecordingURL {
            VStack(alignment: .leading, spacing: 8) {
                Text("DEBUG DATA")
                    .font(.caption.bold().monospaced())
                    .foregroundStyle(.orange)
                SetupCard {
                    ShareLink(item: url) {
                        Label("Export latest recording", systemImage: "square.and.arrow.up")
                    }
                    Text(url.lastPathComponent)
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
            }
        }
    }
}

private struct DiagnosticRecordingListSection: View {
    @ObservedObject var camera: CameraService

    var body: some View {
        if let url = camera.lastRecordingURL {
            Section("DEBUG DATA") {
                ShareLink(item: url) {
                    Label("Export latest recording", systemImage: "square.and.arrow.up")
                }
                Text(url.lastPathComponent)
                    .font(.caption2.monospaced())
                    .foregroundStyle(.secondary)
            }
        }
    }
}
#endif

/// Picks the pattern the player's own silhouette is drawn with on opponents'
/// phones. Shown in the profile and as the last step of enrollment, so a first
/// run never leaves a player on the default without having seen the choice.
private struct SilhouetteSkinPicker: View {
    @ObservedObject var session: AppSession

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("How opponents see you when you are in their sights.")
                .font(.caption)
                .foregroundStyle(.secondary)
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 12) {
                    ForEach(SilhouetteSkin.allCases, id: \.self) { skin in
                        Button {
                            Task { await session.setPreferredSkin(skin) }
                        } label: {
                            VStack(spacing: 5) {
                                SilhouetteSkinSwatch(
                                    skin: skin,
                                    selected: session.preferredSkin == skin
                                )
                                Text(skin.displayName)
                                    .font(.caption2)
                                    .foregroundStyle(
                                        session.preferredSkin == skin ? .primary : .secondary
                                    )
                            }
                        }
                        .buttonStyle(.plain)
                        .accessibilityAddTraits(
                            session.preferredSkin == skin ? [.isSelected] : []
                        )
                    }
                }
                .padding(.vertical, 2)
            }
            .disabled(session.busy)
        }
    }
}

private struct MatchLobbyView: View {
    @ObservedObject var session: AppSession
    @ObservedObject private var game: GameplayCoordinator
    @Environment(\.dismiss) private var dismiss

    init(session: AppSession) {
        self.session = session
        game = session.game
    }

    var body: some View {
        NavigationStack {
            VStack(spacing: 20) {
                Text(game.match?.status == .briefing ? "TARGET BRIEFING" : "MATCH LOBBY")
                    .font(.title.bold().monospaced())
                if game.match?.status == .briefing {
                    briefing
                } else {
                    lobby
                }
                Text(game.statusMessage)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                Spacer()
            }
            .padding(24)
            .background(AppBackground())
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Leave", role: .destructive) {
                        game.leaveMatch()
                        dismiss()
                    }
                }
            }
        }
    }

    private var lobby: some View {
        VStack(spacing: 14) {
            if let code = game.match?.inviteCode {
                Text(code).font(.system(size: 34, weight: .black, design: .monospaced))
                ShareLink(item: code) { Label("Share code", systemImage: "square.and.arrow.up") }
            }
            ForEach(game.match?.players ?? [], id: \.playerId) { player in
                HStack {
                    Image(systemName: player.ready ? "checkmark.circle.fill" : "circle")
                        .foregroundStyle(player.ready ? .green : .orange)
                    Text(player.playerId == game.session?.playerId ? "You" : "Opponent")
                    Spacer()
                    Text(player.ready ? "READY" : "WAITING").font(.caption.monospaced())
                }
                .padding()
                .background(.white.opacity(0.06), in: RoundedRectangle(cornerRadius: 12))
            }
#if DEBUG
            if game.match?.players.count == 1 {
                VStack(alignment: .leading, spacing: 8) {
                    Text("SOLO TEST BOT").font(.caption.bold().monospaced()).foregroundStyle(.orange)
                    Text(game.botLaunchCommand)
                        .font(.caption2.monospaced())
                        .textSelection(.enabled)
                    Toggle("Simulate reciprocal proximity", isOn: Binding(
                        get: { game.botFallbackEnabled },
                        set: game.setBotFallback
                    ))
                }
                .padding()
                .background(.white.opacity(0.06), in: RoundedRectangle(cornerRadius: 12))
            }
#endif
            Button("Ready") { game.ready(setupComplete: session.readiness.canEnterMatch) }
                .buttonStyle(.borderedProminent)
                .tint(.green)
                .disabled(
                    !session.readiness.canEnterMatch
                        || game.myState?.ready == true
                        || game.match?.players.count != 2
                )
        }
    }

    private var briefing: some View {
        VStack(spacing: 14) {
            if let base64 = game.opponentProfile?.briefingThumbnail,
               let data = Data(base64Encoded: base64),
               let image = UIImage(data: data) {
                Image(uiImage: image)
                    .resizable()
                    .scaledToFill()
                    .frame(width: 180, height: 220)
                    .clipShape(RoundedRectangle(cornerRadius: 16))
            }
            Text(game.opponentProfile?.displayName ?? "Opponent").font(.title2.bold())
            Text(game.opponentProfile?.generatedDescription ?? "Acquiring appearance briefing…")
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
            Button("I know my target") { game.acknowledgeBriefing() }
                .buttonStyle(.borderedProminent)
                .tint(.red)
                .disabled(game.myState?.briefingAcknowledged == true)
        }
    }
}

private struct MatchResultView: View {
    @ObservedObject var session: AppSession
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 18) {
            Image(systemName: session.game.match?.winner == session.account?.playerId ? "trophy.fill" : "shield.slash.fill")
                .font(.system(size: 62))
                .foregroundStyle(session.game.match?.winner == session.account?.playerId ? .green : .red)
            Text(session.game.match?.winner == session.account?.playerId ? "YOU WIN" : "MATCH COMPLETE")
                .font(.largeTitle.weight(.black).monospaced())
            Button("View match history") {
                session.game.leaveMatch()
                session.selectedTab = .history
                Task { await session.loadHistory() }
                dismiss()
            }
            .buttonStyle(.borderedProminent)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(AppBackground())
    }
}

private struct LoadStateList<Value, Content: View>: View {
    let state: LoadState<[Value]>
    let empty: String
    @ViewBuilder let content: ([Value]) -> Content

    var body: some View {
        List {
            switch state {
            case .idle, .loading:
                ProgressView()
            case .failed(let message):
                ContentUnavailableView("Unable to load", systemImage: "wifi.exclamationmark", description: Text(message))
            case .loaded(let values):
                if values.isEmpty {
                    ContentUnavailableView(empty, systemImage: "tray")
                } else {
                    content(values)
                }
            }
        }
    }
}

private struct SetupCard<Content: View>: View {
    @ViewBuilder let content: Content

    init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    var body: some View {
        content
            .padding(16)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.white.opacity(0.065), in: RoundedRectangle(cornerRadius: 15))
            .overlay(RoundedRectangle(cornerRadius: 15).stroke(.white.opacity(0.10)))
    }
}

private struct AppBackground: View {
    var body: some View {
        LinearGradient(
            colors: [Color(red: 0.035, green: 0.04, blue: 0.05), .black],
            startPoint: .topLeading,
            endPoint: .bottomTrailing
        )
        .ignoresSafeArea()
    }
}

private struct ProgressScreen: View {
    let message: String

    var body: some View {
        VStack(spacing: 14) {
            ProgressView().controlSize(.large).tint(.red)
            Text(message).font(.headline.monospaced())
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(AppBackground())
    }
}

private struct EnrollmentPhotoPicker: UIViewControllerRepresentable {
    let cameraDevice: UIImagePickerController.CameraDevice
    let onImage: (UIImage) -> Void
    @Environment(\.dismiss) private var dismiss

    func makeCoordinator() -> Coordinator { Coordinator(parent: self) }

    func makeUIViewController(context: Context) -> UIImagePickerController {
        let picker = UIImagePickerController()
        picker.sourceType = UIImagePickerController.isSourceTypeAvailable(.camera) ? .camera : .photoLibrary
        if picker.sourceType == .camera { picker.cameraDevice = cameraDevice }
        picker.delegate = context.coordinator
        return picker
    }

    func updateUIViewController(_ uiViewController: UIImagePickerController, context: Context) {}

    final class Coordinator: NSObject, UINavigationControllerDelegate, UIImagePickerControllerDelegate {
        let parent: EnrollmentPhotoPicker
        init(parent: EnrollmentPhotoPicker) { self.parent = parent }

        func imagePickerController(
            _ picker: UIImagePickerController,
            didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]
        ) {
            if let image = info[.originalImage] as? UIImage { parent.onImage(image) }
            parent.dismiss()
        }

        func imagePickerControllerDidCancel(_ picker: UIImagePickerController) { parent.dismiss() }
    }
}

private extension String {
    var trimmed: String { trimmingCharacters(in: .whitespacesAndNewlines) }
}
