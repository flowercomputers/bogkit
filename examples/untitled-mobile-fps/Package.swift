// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "UntitledMobileFPSCore",
    platforms: [.macOS(.v13), .iOS(.v17)],
    products: [
        .library(name: "UntitledMobileFPS", targets: ["UntitledMobileFPS"])
    ],
    targets: [
        .target(
            name: "UntitledMobileFPS",
            path: "UntitledMobileFPS",
            exclude: [
                "CameraPreview.swift",
                "CameraService.swift",
                "ContentView.swift",
                "DebugOverlay.swift",
                "Info.plist",
                "AppearanceAnalyzer.swift",
                "MobileCLIPEmbedder.swift",
                "OutfitZeroShotClassifier.swift",
                "OutfitLabels.json",
                "AppSession.swift",
                "AppViews.swift",
                "CredentialStore.swift",
                "GameplayCoordinator.swift",
                "MultiplayerViews.swift",
                "NearbyInteractionService.swift",
                "PersonTargetingService.swift",
                "RealtimeMatchClient.swift",
                "MediaPipeHandTracker.swift",
                "ReticleStyle.swift",
                "SightsReticle.swift",
                "SilhouetteSkinRenderer.swift",
                "hand_landmarker.task",
                "MobileCLIPImageEncoder.mlpackage",
                "UntitledMobileFPSApp.swift",
                "VisionHandPoseDetector.swift"
            ],
            sources: [
                "Models.swift",
                "FingerGunClassifier.swift",
                "GestureStateMachine.swift",
                "Aiming.swift",
                "LandmarkRecording.swift",
                "PreviewGeometry.swift",
                "MultiplayerModels.swift",
                "AppearanceMatching.swift",
                "AppModels.swift",
                "SightsAiming.swift",
                "SilhouetteSkin.swift"
            ]
        ),
        .testTarget(
            name: "UntitledMobileFPSTests",
            dependencies: ["UntitledMobileFPS"],
            path: "UntitledMobileFPSTests"
        )
    ]
)
