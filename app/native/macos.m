#include "bridge.h"
#include "tray_icons.h"
#import <AppKit/AppKit.h>
#import <ServiceManagement/ServiceManagement.h>
#import <UserNotifications/UserNotifications.h>
#include <signal.h>
#include <stdlib.h>

static void emit(NSString *kind, NSString *value) {
    runberth_app_event(kind.UTF8String, (value ?: @"").UTF8String);
}

// Sparkle checks for, downloads, and installs updates. Only the Xcode build
// links it; a plain cargo build runs without it and keeps Rust's release
// check, so Sparkle is found at run time and used through these declarations.
@interface NSObject (RunberthSparkle)
- (instancetype)initWithStartingUpdater:(BOOL)startUpdater
                        updaterDelegate:(id)updaterDelegate
                     userDriverDelegate:(id)userDriverDelegate;
- (void)checkForUpdates:(id)sender;
@end

@interface Tray : NSObject <NSApplicationDelegate, NSMenuDelegate, UNUserNotificationCenterDelegate>
@property NSStatusItem *status;
@property NSString *menuJSON;
@property dispatch_source_t terminateSignal;
@property dispatch_source_t interruptSignal;
/* SPUStandardUpdaterController when Sparkle is present, otherwise nil. */
@property id updaterController;
/* Notifications need an app bundle; a bare cargo build has none. */
@property BOOL canNotify;
@end

static Tray *tray;

/* Template images: AppKit inks them for the menu bar's appearance. */
static NSImage *template(const unsigned char *png, size_t length) {
    NSImage *image = [[NSImage alloc] initWithData:[NSData dataWithBytes:png length:length]];
    image.size = NSMakeSize(18, 18);
    image.template = YES;
    image.accessibilityDescription = @"Runberth";
    return image;
}

/* The berth and boat, with a dot while jobs run or "!" after a failed run.
 * An unreachable server keeps the idle icon; the menu and tooltip say so. */
static NSImage *trayIcon(NSString *state) {
    static NSImage *idle, *running, *failed;
    if (!idle) {
        idle = template(tray_idle, sizeof tray_idle);
        running = template(tray_running, sizeof tray_running);
        failed = template(tray_failed, sizeof tray_failed);
    }
    if ([state isEqualToString:@"running"])
        return running;
    if ([state isEqualToString:@"failed"])
        return failed;
    return idle;
}

@implementation Tray
- (void)installSignalHandlers {
    signal(SIGTERM, SIG_IGN);
    signal(SIGINT, SIG_IGN);
    self.terminateSignal =
        dispatch_source_create(DISPATCH_SOURCE_TYPE_SIGNAL, SIGTERM, 0, dispatch_get_main_queue());
    self.interruptSignal =
        dispatch_source_create(DISPATCH_SOURCE_TYPE_SIGNAL, SIGINT, 0, dispatch_get_main_queue());
    dispatch_source_set_event_handler(self.terminateSignal, ^{
      emit(@"quit", @"");
    });
    dispatch_source_set_event_handler(self.interruptSignal, ^{
      emit(@"quit", @"");
    });
    dispatch_resume(self.terminateSignal);
    dispatch_resume(self.interruptSignal);
}
- (void)applicationDidFinishLaunching:(NSNotification *)notification {
    Class sparkle = NSClassFromString(@"SPUStandardUpdaterController");
    if (sparkle)
        self.updaterController = [[sparkle alloc] initWithStartingUpdater:YES
                                                          updaterDelegate:nil
                                                       userDriverDelegate:self];
    if (NSBundle.mainBundle.bundleIdentifier) {
        self.canNotify = YES;
        UNUserNotificationCenter *center = UNUserNotificationCenter.currentNotificationCenter;
        center.delegate = self;
        [center requestAuthorizationWithOptions:UNAuthorizationOptionAlert | UNAuthorizationOptionSound
                              completionHandler:^(BOOL granted, NSError *error) {
                                (void)granted;
                                if (error)
                                    NSLog(@"runberth: notifications unavailable: %@", error);
                              }];
    }
    self.status = [[NSStatusBar systemStatusBar] statusItemWithLength:NSVariableStatusItemLength];
    self.status.autosaveName = @"dev.runberth.app.status";
    self.status.button.image = trayIcon(@"offline");
    self.status.button.imagePosition = NSImageOnly;
    self.status.button.toolTip = @"Runberth — connecting";
    NSMenu *menu = [[NSMenu alloc] initWithTitle:@"Runberth"];
    menu.autoenablesItems = NO;
    menu.delegate = self;
    NSMenuItem *quit = [menu addItemWithTitle:@"Quit" action:@selector(quit:) keyEquivalent:@"q"];
    quit.target = self;
    self.status.menu = menu;
    emit(@"ready", @"");
}
- (void)menuWillOpen:(NSMenu *)menu {
    if (menu == self.status.menu)
        emit(@"menu-opened", @"");
}
- (void)choose:(NSMenuItem *)sender {
    emit(@"click", [NSString stringWithFormat:@"%ld", (long)sender.tag]);
}
- (void)quit:(id)sender {
    emit(@"quit", @"");
}
- (void)fill:(NSMenu *)menu items:(NSArray *)items {
    [menu removeAllItems];
    menu.autoenablesItems = NO;
    for (NSDictionary *spec in items) {
        if ([spec[@"separator"] boolValue]) {
            [menu addItem:[NSMenuItem separatorItem]];
            continue;
        }
        NSMenuItem *item = [menu addItemWithTitle:spec[@"label"] ?: @""
                                           action:@selector(choose:)
                                    keyEquivalent:@""];
        item.target = self;
        item.tag = [spec[@"id"] integerValue];
        item.enabled = [spec[@"enabled"] boolValue];
        if (spec[@"checked"])
            item.state = [spec[@"checked"] boolValue] ? NSControlStateValueOn : NSControlStateValueOff;
        NSArray *children = spec[@"children"];
        if ([children isKindOfClass:NSArray.class]) {
            item.action = nil;
            item.submenu = [[NSMenu alloc] initWithTitle:item.title];
            [self fill:item.submenu items:children];
        }
    }
}
- (void)apply:(NSString *)json {
    if ([json isEqualToString:self.menuJSON])
        return;
    NSDictionary *spec = [NSJSONSerialization JSONObjectWithData:[json dataUsingEncoding:NSUTF8StringEncoding]
                                                         options:0
                                                           error:NULL];
    if (![spec isKindOfClass:NSDictionary.class])
        return;
    self.menuJSON = json;
    NSString *state = spec[@"icon"];
    self.status.button.image = trayIcon(state);
    NSString *badge = spec[@"badge"];
    self.status.button.title = badge.length ? badge : @"";
    self.status.button.imagePosition = badge.length ? NSImageLeft : NSImageOnly;
    self.status.button.toolTip = spec[@"tooltip"];
    [self fill:self.status.menu items:spec[@"items"]];
}
// Show banners even though the app counts as active while its menu is open.
- (void)userNotificationCenter:(UNUserNotificationCenter *)center
       willPresentNotification:(UNNotification *)notification
         withCompletionHandler:(void (^)(UNNotificationPresentationOptions))completionHandler {
    completionHandler(UNNotificationPresentationOptionBanner | UNNotificationPresentationOptionList |
                      UNNotificationPresentationOptionSound);
}
- (void)userNotificationCenter:(UNUserNotificationCenter *)center
    didReceiveNotificationResponse:(UNNotificationResponse *)response
             withCompletionHandler:(void (^)(void))completionHandler {
    NSString *url = response.notification.request.content.userInfo[@"url"];
    if ([response.actionIdentifier isEqualToString:UNNotificationDefaultActionIdentifier] && url.length)
        [NSWorkspace.sharedWorkspace openURL:[NSURL URLWithString:url]];
    completionHandler();
}
// Sparkle's user driver: a menu bar app has no Dock icon to badge, so a
// scheduled update is shown in its own window right away.
- (BOOL)supportsGentleScheduledUpdateReminders {
    return YES;
}
- (BOOL)standardUserDriverShouldHandleShowingScheduledUpdate:(id)update
                                        andInImmediateFocus:(BOOL)immediateFocus {
    return YES;
}
@end

int native_run(void) {
    @autoreleasepool {
        tray = [[Tray alloc] init];
        [tray installSignalHandlers];
        NSApplication *app = [NSApplication sharedApplication];
        [app setActivationPolicy:NSApplicationActivationPolicyAccessory];
        app.delegate = tray;
        [app finishLaunching];
        [app run];
    }
    return 0;
}

void native_stop(void) {
    // Stop the run loop rather than terminate, so Rust can return.
    [NSApp stop:nil];
    [NSApp postEvent:[NSEvent otherEventWithType:NSEventTypeApplicationDefined
                                        location:NSZeroPoint
                                   modifierFlags:0
                                       timestamp:0
                                    windowNumber:0
                                         context:nil
                                         subtype:0
                                           data1:0
                                           data2:0]
             atStart:NO];
}

void native_set_menu(const char *menu_json) {
    NSString *json = @(menu_json);
    dispatch_async(dispatch_get_main_queue(), ^{
      [tray apply:json];
    });
}

void native_notify(const char *title, const char *body, const char *url) {
    NSString *titleText = @(title), *bodyText = @(body), *link = @(url);
    dispatch_async(dispatch_get_main_queue(), ^{
      if (!tray.canNotify) {
          NSLog(@"runberth: %@ — %@", titleText, bodyText);
          return;
      }
      UNMutableNotificationContent *content = [[UNMutableNotificationContent alloc] init];
      content.title = titleText;
      content.body = bodyText;
      content.sound = UNNotificationSound.defaultSound;
      content.userInfo = @{@"url" : link};
      UNNotificationRequest *request =
          [UNNotificationRequest requestWithIdentifier:NSUUID.UUID.UUIDString content:content trigger:nil];
      [UNUserNotificationCenter.currentNotificationCenter addNotificationRequest:request
                                                           withCompletionHandler:^(NSError *error) {
                                                             if (error)
                                                                 NSLog(@"runberth: cannot notify: %@", error);
                                                           }];
    });
}

// SMAppService exists from macOS 13. A runtime class check stands in for
// @available, whose helper lives in clang's runtime that Rust does not link.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wunguarded-availability-new"
int native_login_item(int action) {
    // SMAppService registers the running bundle; a bare binary has none.
    if (!NSClassFromString(@"SMAppService") || ![NSBundle.mainBundle.bundlePath hasSuffix:@".app"])
        return -1;
    SMAppService *service = SMAppService.mainAppService;
    NSError *error = nil;
    if (action == 1 && service.status != SMAppServiceStatusEnabled &&
        service.status != SMAppServiceStatusRequiresApproval && ![service registerAndReturnError:&error])
        NSLog(@"runberth: cannot enable Launch at Login: %@", error);
    else if (action == 0 && service.status != SMAppServiceStatusNotRegistered &&
             ![service unregisterAndReturnError:&error])
        NSLog(@"runberth: cannot disable Launch at Login: %@", error);
    // Registration may need the user's approval; take them straight there.
    if (action == 1 && service.status == SMAppServiceStatusRequiresApproval)
        [SMAppService openSystemSettingsLoginItems];
    switch (service.status) {
    case SMAppServiceStatusEnabled:
        return 1;
    case SMAppServiceStatusRequiresApproval:
        return 2;
    default:
        return 0;
    }
}
#pragma clang diagnostic pop

int native_has_updater(void) {
    return tray.updaterController != nil;
}

void native_check_updates(void) {
    [tray.updaterController checkForUpdates:nil];
}
