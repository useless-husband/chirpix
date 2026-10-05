// Play a WAV file and record at the same time, for the over-the-air test.
//
//   swift scripts/audio_loop.swift --play tx.wav --record rx.wav
//         [--out-device NAME] [--in-device NAME] [--no-prompt]
//
// Without device names it uses the system's current output and input
// (it never changes them, and never touches the volume). With names it
// uses exactly those devices and refuses to run if it cannot: that is how
// the loopback test sends audio through a virtual device ("BlackHole 2ch")
// without making a sound.
//
// Exit codes: 0 ok, 2 bad arguments or files, 3 no microphone permission,
// 4 device not found or could not be selected.

import AVFoundation
import CoreAudio

func fail(_ code: Int32, _ msg: String) -> Never {
    FileHandle.standardError.write((msg + "\n").data(using: .utf8)!)
    exit(code)
}

func allDevices() -> [AudioDeviceID] {
    var addr = AudioObjectPropertyAddress(
        mSelector: kAudioHardwarePropertyDevices, mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain)
    var size: UInt32 = 0
    let sys = AudioObjectID(kAudioObjectSystemObject)
    guard AudioObjectGetPropertyDataSize(sys, &addr, 0, nil, &size) == noErr else { return [] }
    var ids = [AudioDeviceID](repeating: 0, count: Int(size) / MemoryLayout<AudioDeviceID>.size)
    guard AudioObjectGetPropertyData(sys, &addr, 0, nil, &size, &ids) == noErr else { return [] }
    return ids
}

func deviceName(_ id: AudioDeviceID) -> String {
    var addr = AudioObjectPropertyAddress(
        mSelector: kAudioObjectPropertyName, mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain)
    var name: Unmanaged<CFString>?
    var size = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
    guard AudioObjectGetPropertyData(id, &addr, 0, nil, &size, &name) == noErr, let n = name else { return "" }
    return n.takeRetainedValue() as String
}

func channels(_ id: AudioDeviceID, input: Bool) -> Int {
    var addr = AudioObjectPropertyAddress(
        mSelector: kAudioDevicePropertyStreamConfiguration,
        mScope: input ? kAudioDevicePropertyScopeInput : kAudioDevicePropertyScopeOutput,
        mElement: kAudioObjectPropertyElementMain)
    var size: UInt32 = 0
    guard AudioObjectGetPropertyDataSize(id, &addr, 0, nil, &size) == noErr, size > 0 else { return 0 }
    let raw = UnsafeMutableRawPointer.allocate(byteCount: Int(size), alignment: 16)
    defer { raw.deallocate() }
    guard AudioObjectGetPropertyData(id, &addr, 0, nil, &size, raw) == noErr else { return 0 }
    let list = UnsafeMutableAudioBufferListPointer(raw.assumingMemoryBound(to: AudioBufferList.self))
    return list.reduce(0) { $0 + Int($1.mNumberChannels) }
}

func findDevice(_ name: String, input: Bool) -> AudioDeviceID? {
    allDevices().first { deviceName($0) == name && channels($0, input: input) > 0 }
}

func currentDevice(_ unit: AudioUnit) -> AudioDeviceID {
    var id: AudioDeviceID = 0
    var size = UInt32(MemoryLayout<AudioDeviceID>.size)
    AudioUnitGetProperty(unit, kAudioOutputUnitProperty_CurrentDevice, kAudioUnitScope_Global, 0, &id, &size)
    return id
}

func select(_ unit: AudioUnit, _ id: AudioDeviceID) -> Bool {
    var d = id
    let st = AudioUnitSetProperty(
        unit, kAudioOutputUnitProperty_CurrentDevice, kAudioUnitScope_Global, 0, &d,
        UInt32(MemoryLayout<AudioDeviceID>.size))
    return st == noErr && currentDevice(unit) == id
}

var play: String?
var record: String?
var outName: String?
var inName: String?
var noPrompt = false
var args = Array(CommandLine.arguments.dropFirst())
while !args.isEmpty {
    let a = args.removeFirst()
    switch a {
    case "--play": play = args.isEmpty ? nil : args.removeFirst()
    case "--record": record = args.isEmpty ? nil : args.removeFirst()
    case "--out-device": outName = args.isEmpty ? nil : args.removeFirst()
    case "--in-device": inName = args.isEmpty ? nil : args.removeFirst()
    case "--no-prompt": noPrompt = true
    default: fail(2, "unknown argument \(a)")
    }
}
guard let playPath = play, let recordPath = record else {
    fail(2, "usage: audio_loop.swift --play tx.wav --record rx.wav [--out-device NAME] [--in-device NAME] [--no-prompt]")
}

// Microphone permission. macOS asks once per application (here: the
// terminal program this script runs in).
switch AVCaptureDevice.authorizationStatus(for: .audio) {
case .authorized: break
case .notDetermined:
    if noPrompt { fail(3, "microphone permission has not been granted yet (and --no-prompt was given)") }
    let sem = DispatchSemaphore(value: 0)
    var ok = false
    AVCaptureDevice.requestAccess(for: .audio) { granted in
        ok = granted
        sem.signal()
    }
    sem.wait()
    if !ok { fail(3, "microphone permission was refused") }
default:
    fail(3, "microphone permission is denied for this terminal (System Settings > Privacy & Security > Microphone)")
}

let engine = AVAudioEngine()
let player = AVAudioPlayerNode()
let file: AVAudioFile
do { file = try AVAudioFile(forReading: URL(fileURLWithPath: playPath)) } catch { fail(2, "cannot open \(playPath): \(error.localizedDescription)") }

var wantOut: AudioDeviceID?
if let name = outName {
    guard let id = findDevice(name, input: false) else { fail(4, "no output device named \"\(name)\"") }
    guard let unit = engine.outputNode.audioUnit, select(unit, id) else { fail(4, "could not select output device \"\(name)\"") }
    wantOut = id
}
// Recording goes through a capture session bound to one device, so the
// system's default input is neither needed nor changed.
final class Finish: NSObject, AVCaptureFileOutputRecordingDelegate {
    var error: Error?
    let done = DispatchSemaphore(value: 0)
    func fileOutput(
        _ output: AVCaptureFileOutput, didFinishRecordingTo outputFileURL: URL, from connections: [AVCaptureConnection],
        error: Error?
    ) {
        self.error = error
        done.signal()
    }
}
let mics = AVCaptureDevice.DiscoverySession(
    deviceTypes: [.microphone, .external], mediaType: .audio, position: .unspecified
).devices
let mic: AVCaptureDevice
if let name = inName {
    guard let d = mics.first(where: { $0.localizedName == name }) else {
        fail(4, "no input device named \"\(name)\" (found: \(mics.map { $0.localizedName }.joined(separator: ", ")))")
    }
    mic = d
} else {
    guard let d = AVCaptureDevice.default(for: .audio) else { fail(4, "no audio input device") }
    mic = d
}
let session = AVCaptureSession()
let capture = AVCaptureAudioFileOutput()
do {
    let input = try AVCaptureDeviceInput(device: mic)
    guard session.canAddInput(input), session.canAddOutput(capture) else { fail(4, "cannot record from \(mic.localizedName)") }
    session.addInput(input)
    session.addOutput(capture)
} catch { fail(4, "cannot open \(mic.localizedName): \(error.localizedDescription)") }
capture.audioSettings = [
    AVFormatIDKey: kAudioFormatLinearPCM,
    AVLinearPCMBitDepthKey: 16,
    AVLinearPCMIsFloatKey: false,
    AVLinearPCMIsBigEndianKey: false,
    AVLinearPCMIsNonInterleaved: false,
]
let finish = Finish()
let recordURL = URL(fileURLWithPath: recordPath)
try? FileManager.default.removeItem(at: recordURL)

engine.attach(player)
engine.connect(player, to: engine.mainMixerNode, format: file.processingFormat)
engine.prepare()
do { try engine.start() } catch { fail(4, "audio engine did not start: \(error.localizedDescription)") }

// Last check before any sound: if a named output was asked for, it must
// still be the one in use.
if let id = wantOut, let unit = engine.outputNode.audioUnit, currentDevice(unit) != id {
    engine.stop()
    fail(4, "output device changed unexpectedly; not playing")
}

let seconds = Double(file.length) / file.processingFormat.sampleRate
session.startRunning()
capture.startRecording(to: recordURL, outputFileType: .wav, recordingDelegate: finish)
Thread.sleep(forTimeInterval: 0.5)
print(String(format: "playing %.1f s, recording from %@", seconds, mic.localizedName))
player.scheduleFile(file, at: nil, completionHandler: nil)
player.play()
Thread.sleep(forTimeInterval: seconds + 1.0)
player.stop()
engine.stop()
capture.stopRecording()
_ = finish.done.wait(timeout: .now() + 10)
session.stopRunning()
if let e = finish.error { fail(4, "recording failed: \(e.localizedDescription)") }
print("recorded \(recordPath)")
