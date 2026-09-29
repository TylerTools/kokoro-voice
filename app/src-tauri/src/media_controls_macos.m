// Owns HereWord's accessory commands and generic Now Playing metadata only.
// Selected text never enters system metadata; an idle session releases ownership.
#import <Foundation/Foundation.h>
#import <MediaPlayer/MediaPlayer.h>

typedef bool (*HereWordMediaHandler)(int);
static HereWordMediaHandler handler;
static NSMutableArray *targets;

void hereword_media_controls_init(HereWordMediaHandler callback) {
    handler = callback;
    targets = [NSMutableArray array];
    MPRemoteCommandCenter *center = MPRemoteCommandCenter.sharedCommandCenter;
    NSArray<MPRemoteCommand *> *commands = @[
        center.playCommand, center.pauseCommand,
        center.togglePlayPauseCommand, center.stopCommand
    ];
    for (NSUInteger i = 0; i < commands.count; i++) {
        MPRemoteCommand *command = commands[i];
        command.enabled = NO;
        int action = (int)i;
        id target = [command addTargetWithHandler:^MPRemoteCommandHandlerStatus(MPRemoteCommandEvent *event) {
            return handler && handler(action) ? MPRemoteCommandHandlerStatusSuccess
                                             : MPRemoteCommandHandlerStatusNoSuchContent;
        }];
        [targets addObject:target];
    }
    center.nextTrackCommand.enabled = NO;
    center.previousTrackCommand.enabled = NO;
    center.skipForwardCommand.enabled = NO;
    center.skipBackwardCommand.enabled = NO;
    center.seekForwardCommand.enabled = NO;
    center.seekBackwardCommand.enabled = NO;
}

void hereword_media_controls_update(int state) {
    MPRemoteCommandCenter *commands = MPRemoteCommandCenter.sharedCommandCenter;
    BOOL active = state == 1 || state == 2;
    commands.playCommand.enabled = active;
    commands.pauseCommand.enabled = active;
    commands.togglePlayPauseCommand.enabled = active;
    commands.stopCommand.enabled = active;
    MPNowPlayingInfoCenter *info = MPNowPlayingInfoCenter.defaultCenter;
    if (!active) {
        info.playbackState = MPNowPlayingPlaybackStateStopped;
        info.nowPlayingInfo = nil;
        return;
    }
    info.nowPlayingInfo = @{
        MPMediaItemPropertyTitle: @"HereWord Reading",
        MPMediaItemPropertyArtist: @"HereWord",
        MPNowPlayingInfoPropertyMediaType: @(MPNowPlayingInfoMediaTypeAudio),
        MPNowPlayingInfoPropertyPlaybackRate: @(state == 1 ? 1.0 : 0.0)
    };
    info.playbackState = state == 1 ? MPNowPlayingPlaybackStatePlaying
                                  : MPNowPlayingPlaybackStatePaused;
}
