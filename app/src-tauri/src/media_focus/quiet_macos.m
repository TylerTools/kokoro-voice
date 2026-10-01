// Owns a private, muted Core Audio tap of external audio processes only.
// Never reads, saves, or forwards audio. Destroying the tap restores output;
// never change a device's master volume or include HereWord's descendants.
#import <Foundation/Foundation.h>
#import <CoreAudio/CoreAudio.h>
#import <CoreAudio/CATapDescription.h>
#import <CoreAudio/AudioHardwareTapping.h>
#include <libproc.h>
#include <unistd.h>
#include <string.h>

@interface HWQuiet : NSObject
@property AudioObjectID tap;
@property (strong) NSArray<NSNumber *> *processes;
@property (strong) CATapDescription *tapDescription;
@end
@implementation HWQuiet
@end

static BOOL ownedProcess(pid_t pid) {
    pid_t root = getpid();
    for (unsigned n = 0; pid > 1 && n < 64; n++) {
        if (pid == root) return YES;
        char path[PROC_PIDPATHINFO_MAXSIZE] = {0};
        if (proc_pidpath(pid, path, sizeof(path)) > 0 &&
            strncmp(path, "/Applications/Kokoro Voice 2.app/", strlen("/Applications/Kokoro Voice 2.app/")) == 0)
            return YES;
        struct proc_bsdinfo info = {0};
        if (proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, sizeof(info)) != sizeof(info))
            return YES; // Unknown ancestry cannot safely be silenced.
        if (info.pbi_ppid == (uint32_t)pid) return YES;
        pid = (pid_t)info.pbi_ppid;
    }
    return NO;
}

static NSArray<NSNumber *> *externalProcesses(void) {
    AudioObjectPropertyAddress address = {
        kAudioHardwarePropertyProcessObjectList, kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyElementMain
    };
    UInt32 size = 0;
    if (AudioObjectGetPropertyDataSize(kAudioObjectSystemObject, &address, 0, NULL, &size) != noErr)
        return nil;
    NSMutableData *data = [NSMutableData dataWithLength:size];
    if (AudioObjectGetPropertyData(kAudioObjectSystemObject, &address, 0, NULL, &size, data.mutableBytes) != noErr)
        return nil;
    AudioObjectID *objects = data.mutableBytes;
    NSMutableArray<NSNumber *> *result = [NSMutableArray array];
    address.mSelector = kAudioProcessPropertyPID;
    for (UInt32 n = 0; n < size / sizeof(AudioObjectID); n++) {
        pid_t pid = 0;
        UInt32 pidSize = sizeof(pid);
        if (AudioObjectGetPropertyData(objects[n], &address, 0, NULL, &pidSize, &pid) == noErr &&
            pid > 1 && !ownedProcess(pid)) [result addObject:@(objects[n])];
    }
    return result;
}

static OSStatus refresh(HWQuiet *state) {
    if (@available(macOS 14.2, *)) {
        NSArray<NSNumber *> *processes = externalProcesses();
        if (!processes) return kAudioHardwareUnspecifiedError;
        if ([state.processes isEqualToArray:processes]) return noErr;
        if (processes.count == 0) {
            if (state.tap) AudioHardwareDestroyProcessTap(state.tap);
            state.tap = 0;
            state.processes = processes;
            return noErr;
        }
        CATapDescription *description = state.tapDescription;
        if (!description) {
            description = [[CATapDescription alloc] initStereoMixdownOfProcesses:processes];
            description.name = @"HereWord temporary audio quieting";
            description.privateTap = YES;
            description.muteBehavior = CATapMuted;
            state.tapDescription = description;
        } else description.processes = processes;
        OSStatus status;
        if (!state.tap) {
            AudioObjectID tap = 0;
            status = AudioHardwareCreateProcessTap(description, &tap);
            if (status == noErr) state.tap = tap;
        } else {
            AudioObjectPropertyAddress address = {
                kAudioTapPropertyDescription, kAudioObjectPropertyScopeGlobal,
                kAudioObjectPropertyElementMain
            };
            status = AudioObjectSetPropertyData(state.tap, &address, 0, NULL, sizeof(description), &description);
        }
        if (status == noErr) state.processes = processes;
        return status;
    }
    return kAudioHardwareUnsupportedOperationError;
}

void *hereword_quiet_start(int32_t *status) {
    @autoreleasepool {
      @try {
        HWQuiet *state = [HWQuiet new];
        *status = refresh(state);
        if (*status != noErr) return NULL;
        return (__bridge_retained void *)state;
      } @catch (NSException *exception) {
        fprintf(stderr, "HereWord Core Audio: %s: %s\n", exception.name.UTF8String, exception.reason.UTF8String);
        *status = kAudioHardwareUnspecifiedError;
        return NULL;
      }
    }
}
int32_t hereword_quiet_refresh(void *handle) {
    @autoreleasepool {
      @try { return refresh((__bridge HWQuiet *)handle); }
      @catch (NSException *exception) { return kAudioHardwareUnspecifiedError; }
    }
}
void hereword_quiet_stop(void *handle) {
    @autoreleasepool {
        HWQuiet *state = (__bridge_transfer HWQuiet *)handle;
      @try {
        if (@available(macOS 14.2, *)) {
            if (state.tap) AudioHardwareDestroyProcessTap(state.tap);
        }
      } @catch (NSException *exception) { /* Preserve the rest of shutdown. */ }
    }
}
