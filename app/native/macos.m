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
/* The icon state from Rust, and the spinner's turn while jobs run. */
@property NSString *iconState;
@property CGFloat spin;
@property NSTimer *spinTimer;
/* Notifications need an app bundle; a bare cargo build has none. */
@property BOOL canNotify;
@end

@class Settings;
static Settings *settings;

static Tray *tray;

static NSImage *layer(const unsigned char *png, size_t length) {
    NSImage *image = [[NSImage alloc] initWithData:[NSData dataWithBytes:png length:length]];
    image.size = NSMakeSize(20, 20);
    return image;
}

/* The berth and boat in the menu bar's text colour, with a coloured badge in
 * the berth's entrance: a grey cross when the server is unreachable, a green
 * check when the latest runs passed, a red "!" when one failed, and an orange
 * spinner, turned `angle` degrees, while jobs run. "idle" has no badge. */
static NSImage *trayIcon(NSString *state, CGFloat angle) {
    static NSImage *glyph, *cut, *success, *failed, *offline, *running;
    if (!glyph) {
        glyph = layer(tray_glyph, sizeof tray_glyph);
        cut = layer(tray_glyph_cut, sizeof tray_glyph_cut);
        success = layer(tray_success, sizeof tray_success);
        failed = layer(tray_failed, sizeof tray_failed);
        offline = layer(tray_offline, sizeof tray_offline);
        running = layer(tray_running, sizeof tray_running);
    }
    NSDictionary *badges = @{@"success" : success, @"failed" : failed, @"offline" : offline, @"running" : running};
    NSImage *badge = badges[state];
    NSImage *shape = badge ? cut : glyph;
    // Not a template, so the badge keeps its colour: the handler tints the
    // glyph for the appearance it is drawn in, light or dark menu bar.
    NSImage *icon = [NSImage imageWithSize:NSMakeSize(20, 20)
                                   flipped:NO
                            drawingHandler:^BOOL(NSRect rect) {
                              CGContextRef context = NSGraphicsContext.currentContext.CGContext;
                              CGContextBeginTransparencyLayer(context, NULL);
                              [shape drawInRect:rect];
                              [NSColor.labelColor set];
                              NSRectFillUsingOperation(rect, NSCompositingOperationSourceIn);
                              CGContextEndTransparencyLayer(context);
                              if (!badge)
                                  return YES;
                              if (angle != 0) {
                                  // The badge's centre on the 24-unit grid, (19, 18.9), at 20
                                  // points: x 19 * 20/24, y (24 - 18.9) * 20/24 from the bottom.
                                  NSAffineTransform *turn = [NSAffineTransform transform];
                                  [turn translateXBy:15.833 yBy:4.25];
                                  [turn rotateByDegrees:-angle];
                                  [turn translateXBy:-15.833 yBy:-4.25];
                                  [turn concat];
                              }
                              [badge drawInRect:rect];
                              return YES;
                            }];
    icon.cacheMode = NSImageCacheNever;
    icon.accessibilityDescription = @"Runberth";
    return icon;
}


/* ---- Settings window ---------------------------------------------------- */

static NSButton *pushButton(NSString *title, id target, SEL action) {
    NSButton *button = [NSButton buttonWithTitle:title target:target action:action];
    button.bezelStyle = NSBezelStyleRounded;
    return button;
}

/* Secondary text in a fixed-width column, so changing it wraps the text
 * instead of resizing the window. */
static NSTextField *noteLabel(void) {
    NSTextField *label = [NSTextField wrappingLabelWithString:@""];
    label.font = [NSFont systemFontOfSize:NSFont.smallSystemFontSize];
    label.textColor = NSColor.secondaryLabelColor;
    label.preferredMaxLayoutWidth = 300;
    [label.widthAnchor constraintEqualToConstant:300].active = YES;
    return label;
}

static NSTextField *textField(NSString *placeholder, CGFloat width, id delegate) {
    NSTextField *field = [NSTextField textFieldWithString:@""];
    field.placeholderString = placeholder;
    field.delegate = delegate;
    [field.widthAnchor constraintEqualToConstant:width].active = YES;
    return field;
}

static NSStackView *row(NSArray<NSView *> *views) {
    NSStackView *stack = [NSStackView stackViewWithViews:views];
    stack.spacing = 8;
    return stack;
}

static NSStackView *column(NSArray<NSView *> *views) {
    NSStackView *stack = [NSStackView stackViewWithViews:views];
    stack.orientation = NSUserInterfaceLayoutOrientationVertical;
    stack.alignment = NSLayoutAttributeLeading;
    stack.spacing = 6;
    return stack;
}

/* Label-and-control rows with right-aligned labels. NSNull is a separator. */
static NSGridView *form(NSArray *rows) {
    NSGridView *grid = [[NSGridView alloc] init];
    grid.rowSpacing = 12;
    grid.columnSpacing = 8;
    for (id spec in rows) {
        if (spec == NSNull.null) {
            NSBox *line = [[NSBox alloc] init];
            line.boxType = NSBoxSeparator;
            NSGridRow *separator = [grid addRowWithViews:@[ line, NSGridCell.emptyContentView ]];
            [separator mergeCellsInRange:NSMakeRange(0, 2)];
            separator.topPadding = separator.bottomPadding = 4;
            continue;
        }
        NSString *title = spec[0];
        NSView *label = title.length ? [NSTextField labelWithString:title] : NSGridCell.emptyContentView;
        [grid addRowWithViews:@[ label, spec[1] ]];
    }
    [grid columnAtIndex:0].xPlacement = NSGridCellPlacementTrailing;
    grid.rowAlignment = NSGridRowAlignmentFirstBaseline;
    return grid;
}

static NSString *const DonateURL = @"https://ko-fi.com/vantalter";

static NSImage *symbol(NSString *name, NSString *description) {
    return [NSImage imageWithSystemSymbolName:name accessibilityDescription:description];
}

/* A cup with a heart, in the outline style of the other tab symbols. */
static NSImage *donateIcon(void) {
    NSImage *icon = [NSImage
         imageWithSize:NSMakeSize(22, 20)
               flipped:NO
        drawingHandler:^BOOL(NSRect rect) {
          (void)rect;
          [NSColor.blackColor set];
          NSBezierPath *handle = [NSBezierPath bezierPathWithRoundedRect:NSMakeRect(13.5, 7, 6.5, 7.5)
                                                                 xRadius:3.25
                                                                 yRadius:3.25];
          handle.lineWidth = 1.5;
          [handle stroke];
          NSRect cup = NSMakeRect(2.5, 3.5, 13.5, 12.5);
          NSBezierPath *body = [NSBezierPath bezierPathWithRoundedRect:cup xRadius:4 yRadius:4];
          // Clear the handle's inner half so it joins the cup's outline.
          NSGraphicsContext.currentContext.compositingOperation = NSCompositingOperationClear;
          [body fill];
          NSGraphicsContext.currentContext.compositingOperation = NSCompositingOperationSourceOver;
          body.lineWidth = 1.5;
          [body stroke];
          // Two lobes and a point, filled separately so their overlap stays solid.
          CGFloat x = NSMidX(cup), y = NSMidY(cup) + 0.4, r = 2;
          [[NSBezierPath bezierPathWithOvalInRect:NSMakeRect(x - 1.6 - r, y + 0.6 - r, 2 * r, 2 * r)] fill];
          [[NSBezierPath bezierPathWithOvalInRect:NSMakeRect(x + 1.6 - r, y + 0.6 - r, 2 * r, 2 * r)] fill];
          NSBezierPath *point = [NSBezierPath bezierPath];
          [point moveToPoint:NSMakePoint(x - 3.45, y - 0.1)];
          [point lineToPoint:NSMakePoint(x + 3.45, y - 0.1)];
          [point lineToPoint:NSMakePoint(x, y - 3.9)];
          [point closePath];
          [point fill];
          return YES;
        }];
    icon.template = YES;
    icon.accessibilityDescription = @"Donate";
    return icon;
}

static NSTabViewItem *pane(NSString *title, NSImage *image, NSView *content) {
    NSViewController *controller = [[NSViewController alloc] init];
    controller.title = title;
    NSView *view = [[NSView alloc] init];
    content.translatesAutoresizingMaskIntoConstraints = NO;
    [view addSubview:content];
    [NSLayoutConstraint activateConstraints:@[
        [content.topAnchor constraintEqualToAnchor:view.topAnchor constant:20],
        [content.bottomAnchor constraintEqualToAnchor:view.bottomAnchor constant:-20],
        [content.leadingAnchor constraintGreaterThanOrEqualToAnchor:view.leadingAnchor constant:30],
        [content.centerXAnchor constraintEqualToAnchor:view.centerXAnchor],
        [view.widthAnchor constraintEqualToConstant:540],
    ]];
    controller.view = view;
    [view layoutSubtreeIfNeeded];
    controller.preferredContentSize = view.fittingSize;
    NSTabViewItem *item = [NSTabViewItem tabViewItemWithViewController:controller];
    item.image = image;
    return item;
}

static void setStatus(NSTextField *label, NSString *text, BOOL ok) {
    label.stringValue = text ?: @"";
    label.textColor = ok ? NSColor.secondaryLabelColor : NSColor.systemRedColor;
    label.hidden = !label.stringValue.length;
}

/* Server, GitHub, and General tabs. Edits are saved to settings.json shortly
 * after typing stops; Rust validates them and reports problems back. */
@interface Settings : NSObject <NSWindowDelegate, NSTextFieldDelegate>
@property NSWindow *window;
@property NSTextField *host, *user, *port, *key, *tokenCommand, *interval;
@property NSSecureTextField *token;
@property NSPopUpButton *notifications;
@property NSButton *loginBox, *updatesBox, *checkNow;
@property NSPopUpButton *checkInterval;
@property NSTextField *lastChecked;
/* The rows that only Sparkle can fill: the check interval and last check. */
@property NSArray<NSGridRow *> *sparkleRows;
@property NSTextField *testStatus, *tokenState, *tokenMessage, *loginNote, *version, *settingsPath;
@property NSTextField *cleanupStatus;
@property NSButton *cleanupButton;
@property NSArray<NSTextField *> *errorLabels;
@property NSTimer *saveTimer;
@end

@implementation Settings
- (NSTextField *)errorLabel {
    NSTextField *label = noteLabel();
    label.hidden = YES;
    return label;
}
- (NSView *)server {
    self.host = textField(@"203.0.113.10 or vps.example.com", 260, self);
    self.user = textField(@"ubuntu", 140, self);
    self.port = textField(@"22", 70, self);
    self.key = textField(@"~/.ssh/id_ed25519", 260, self);
    NSTextField *keyNote = noteLabel();
    keyNote.stringValue = @"Leave empty to use your SSH agent and ~/.ssh/config. For localhost on port 22 "
                          @"the app runs Runberth on this computer, without SSH. The server's host key "
                          @"must already be known: connect once with ssh first.";
    self.testStatus = noteLabel();
    self.testStatus.hidden = YES;
    NSTextField *error = [self errorLabel];
    self.errorLabels = [(self.errorLabels ?: @[]) arrayByAddingObject:error];
    self.cleanupButton = pushButton(@"Clean Up Server…", self, @selector(cleanup:));
    self.cleanupButton.hasDestructiveAction = YES;
    NSTextField *cleanupNote = noteLabel();
    cleanupNote.stringValue = @"Removes Runberth from the server: every runner and its GitHub registration, "
                              @"their accounts and caches, and the packages and swap it added. Shows the "
                              @"plan before anything is removed.";
    self.cleanupStatus = noteLabel();
    self.cleanupStatus.hidden = YES;
    return form(@[
        @[ @"Host:", self.host ],
        @[ @"User:", self.user ],
        @[ @"Port:", self.port ],
        @[ @"SSH key:", column(@[ row(@[ self.key, pushButton(@"Choose…", self, @selector(chooseKey:)) ]),
                                  keyNote ]) ],
        NSNull.null,
        @[ @"", column(@[ pushButton(@"Test Connection", tray, @selector(testConnection:)), self.testStatus,
                          error ]) ],
        NSNull.null,
        @[ @"Remove:", column(@[ self.cleanupButton, cleanupNote, self.cleanupStatus ]) ],
    ]);
}
- (NSView *)github {
    self.tokenState = [NSTextField labelWithString:@""];
    self.token = [[NSSecureTextField alloc] init];
    self.token.placeholderString = @"github_pat_… or ghp_…";
    [self.token.widthAnchor constraintEqualToConstant:260].active = YES;
    self.tokenMessage = noteLabel();
    self.tokenMessage.hidden = YES;
    NSTextField *tokenNote = noteLabel();
    tokenNote.stringValue = @"Showing runs needs Actions: Read. Connecting and disconnecting repositories "
                            @"also needs Administration: Read and write, and Contents: Read. A classic "
                            @"token with the repo scope covers both. It is kept in your login keychain.";
    self.tokenCommand = textField(@"gh auth token --user NAME", 260, self);
    NSTextField *commandNote = noteLabel();
    commandNote.stringValue = @"Optional. When set, the app runs this command for the token instead of "
                              @"reading the keychain.";
    return form(@[
        @[ @"Token:", self.tokenState ],
        @[ @"New token:", column(@[ row(@[ self.token, pushButton(@"Save", self, @selector(saveToken:)) ]),
                                    tokenNote ]) ],
        @[ @"", column(@[ row(@[ pushButton(@"Create a Token…", self, @selector(createToken:)),
                                 pushButton(@"Remove Token", self, @selector(removeToken:)) ]),
                          self.tokenMessage ]) ],
        NSNull.null,
        @[ @"Token command:", column(@[ self.tokenCommand, commandNote ]) ],
    ]);
}
- (NSView *)general {
    self.interval = textField(@"20", 60, self);
    self.notifications = [[NSPopUpButton alloc] init];
    for (NSArray *choice in @[ @[ @"All finished runs", @"all" ], @[ @"Failed runs only", @"failures" ],
                               @[ @"Off", @"off" ] ]) {
        [self.notifications addItemWithTitle:choice[0]];
        self.notifications.lastItem.representedObject = choice[1];
    }
    self.notifications.target = self;
    self.notifications.action = @selector(changed:);
    self.loginBox = [NSButton checkboxWithTitle:@"Launch at Login" target:self action:@selector(toggleLogin:)];
    self.loginNote = noteLabel();
    self.loginNote.hidden = YES;
    NSTextField *error = [self errorLabel];
    self.errorLabels = [(self.errorLabels ?: @[]) arrayByAddingObject:error];
    // Selectable, so the path can be copied.
    self.settingsPath = noteLabel();
    self.settingsPath.selectable = YES;
    return form(@[
        @[ @"Refresh every:", column(@[ row(@[ self.interval, [NSTextField labelWithString:@"seconds"] ]), error ]) ],
        @[ @"Notifications:", self.notifications ],
        @[ @"Startup:", column(@[ self.loginBox, self.loginNote ]) ],
        NSNull.null,
        @[ @"Settings file:", column(@[ self.settingsPath, pushButton(@"Open settings.json", self, @selector(openFile:)) ]) ],
    ]);
}
- (id)updater {
    return [tray.updaterController valueForKey:@"updater"];
}
- (NSView *)updates {
    self.updatesBox = [NSButton checkboxWithTitle:@"Automatically check for updates"
                                           target:self
                                           action:@selector(changed:)];
    self.checkInterval = [[NSPopUpButton alloc] init];
    for (NSArray *choice in @[ @[ @"Daily", @86400 ], @[ @"Weekly", @604800 ], @[ @"Monthly", @2592000 ] ]) {
        [self.checkInterval addItemWithTitle:choice[0]];
        self.checkInterval.lastItem.tag = [choice[1] integerValue];
    }
    self.checkInterval.target = self;
    self.checkInterval.action = @selector(chooseInterval:);
    self.lastChecked = [NSTextField labelWithString:@""];
    self.checkNow = pushButton(@"Check Now…", self, @selector(checkNow:));
    self.version = [NSTextField labelWithString:@""];
    NSGridView *grid = form(@[
        @[ @"Software Update:", self.updatesBox ],
        @[ @"", self.checkInterval ],
        @[ @"Last checked:", self.lastChecked ],
        @[ @"", self.checkNow ],
        NSNull.null,
        @[ @"Version:", self.version ],
    ]);
    // Without Sparkle, the app checks daily and keeps no check history.
    self.sparkleRows = @[ [grid rowAtIndex:1], [grid rowAtIndex:2] ];
    return grid;
}
- (NSView *)donate {
    // Ko-fi's logo from the app's asset catalog; a build without one shows a cup.
    NSImage *logo = [NSImage imageNamed:@"KoFi"];
    NSImageView *cup;
    if (logo) {
        cup = [NSImageView imageViewWithImage:logo];
        [cup.widthAnchor constraintEqualToConstant:50].active = YES;
        [cup.heightAnchor constraintEqualToAnchor:cup.widthAnchor multiplier:logo.size.height / logo.size.width]
            .active = YES;
    } else {
        NSImageSymbolConfiguration *large = [NSImageSymbolConfiguration configurationWithPointSize:34
                                                                                            weight:NSFontWeightRegular];
        cup = [NSImageView imageViewWithImage:[symbol(@"cup.and.saucer.fill", @"Ko-fi")
                                                  imageWithSymbolConfiguration:large]];
        cup.contentTintColor = NSColor.controlAccentColor;
    }
    cup.imageScaling = NSImageScaleProportionallyUpOrDown;
    NSButton *support = pushButton(@"Support", self, @selector(openDonate:));
    support.keyEquivalent = @"\r";
    NSTextField *name = [NSTextField labelWithString:@"Ko-fi"];
    name.font = [NSFont boldSystemFontOfSize:NSFont.systemFontSize];
    NSStackView *kofi = [NSStackView stackViewWithViews:@[ cup, name, support ]];
    kofi.orientation = NSUserInterfaceLayoutOrientationVertical;
    kofi.spacing = 8;
    NSTextField *free = [NSTextField wrappingLabelWithString:@"Runberth is free and open source, and every feature "
                                                             @"is available to everyone."];
    NSTextField *ask = [NSTextField wrappingLabelWithString:@"Donations help keep it developed. If you would like to "
                                                            @"support the work, you can buy a coffee on Ko-fi."];
    free.preferredMaxLayoutWidth = ask.preferredMaxLayoutWidth = 400;
    NSStackView *stack = column(@[ free, ask, kofi ]);
    stack.spacing = 16;
    [stack setCustomSpacing:24 afterView:ask];
    [kofi.centerXAnchor constraintEqualToAnchor:stack.centerXAnchor].active = YES;
    return stack;
}
/* Sparkle's schedule and history, read again whenever the window shows. */
- (void)refreshUpdates {
    id updater = [self updater];
    for (NSGridRow *gridRow in self.sparkleRows)
        gridRow.hidden = !updater;
    if (!updater)
        return;
    [self.checkInterval selectItemWithTag:[[updater valueForKey:@"updateCheckInterval"] integerValue]];
    if (self.checkInterval.indexOfSelectedItem < 0)
        [self.checkInterval selectItemAtIndex:0];
    self.checkInterval.enabled = self.updatesBox.state == NSControlStateValueOn;
    NSDate *date = [updater valueForKey:@"lastUpdateCheckDate"];
    self.lastChecked.stringValue = date ? [NSDateFormatter localizedStringFromDate:date
                                                                         dateStyle:NSDateFormatterMediumStyle
                                                                         timeStyle:NSDateFormatterMediumStyle]
                                        : @"Never";
}
- (void)windowDidBecomeKey:(NSNotification *)notification {
    [self refreshUpdates];
}
- (void)chooseInterval:(NSPopUpButton *)sender {
    [[self updater] setValue:@((double)sender.selectedTag) forKey:@"updateCheckInterval"];
}
- (void)openDonate:(id)sender {
    [NSWorkspace.sharedWorkspace openURL:[NSURL URLWithString:DonateURL]];
}
- (void)build {
    NSTabViewController *tabs = [[NSTabViewController alloc] init];
    tabs.tabStyle = NSTabViewControllerTabStyleToolbar;
    [tabs addTabViewItem:pane(@"Server", symbol(@"server.rack", @"Server"), [self server])];
    [tabs addTabViewItem:pane(@"GitHub", symbol(@"key", @"GitHub"), [self github])];
    [tabs addTabViewItem:pane(@"General", symbol(@"gearshape", @"General"), [self general])];
    [tabs addTabViewItem:pane(@"Updates", symbol(@"arrow.triangle.2.circlepath", @"Updates"), [self updates])];
    [tabs addTabViewItem:pane(@"Donate", donateIcon(), [self donate])];
    self.window = [NSWindow windowWithContentViewController:tabs];
    self.window.styleMask = NSWindowStyleMaskTitled | NSWindowStyleMaskClosable;
    self.window.releasedWhenClosed = NO;
    self.window.delegate = self;
    [self.window center];
}
- (void)show:(NSDictionary *)state {
    if (!self.window)
        [self build];
    // An open window keeps what is being typed; a closed one shows the file.
    if (!self.window.visible) {
        NSDictionary *fields = state[@"fields"];
        self.host.stringValue = fields[@"host"] ?: @"";
        self.user.stringValue = fields[@"user"] ?: @"";
        self.port.stringValue = fields[@"port"] ?: @"";
        self.key.stringValue = fields[@"key"] ?: @"";
        self.tokenCommand.stringValue = fields[@"token_command"] ?: @"";
        self.interval.stringValue = fields[@"interval"] ?: @"";
        for (NSMenuItem *item in self.notifications.itemArray)
            if ([item.representedObject isEqual:fields[@"notifications"]])
                [self.notifications selectItem:item];
        self.updatesBox.state = [fields[@"check_updates"] boolValue] ? NSControlStateValueOn : NSControlStateValueOff;
        self.token.stringValue = @"";
        self.testStatus.hidden = self.tokenMessage.hidden = YES;
        self.version.stringValue = [NSString stringWithFormat:@"%@", state[@"version"]];
        // With the home folder as ~, as people write it.
        self.settingsPath.stringValue = [state[@"path"] ?: @"" stringByAbbreviatingWithTildeInPath];
    }
    [self refreshUpdates];
    [self status:state];
    [NSApp activateIgnoringOtherApps:YES];
    [self.window makeKeyAndOrderFront:nil];
    // Since macOS 14 an app cannot always activate itself; show the window anyway.
    [self.window orderFrontRegardless];
}
/* Updates the status lines that the dictionary names. */
- (void)cleanup:(id)sender {
    // Clean up the server the window shows, not an edit still being typed.
    [self save];
    self.cleanupButton.enabled = NO;
    setStatus(self.cleanupStatus, @"Reading the cleanup plan…", YES);
    emit(@"cleanup-preview", @"");
}
/* Shows the server's dry-run plan and removes everything only on a second,
 * explicit choice: cleanup cannot be undone. */
- (void)confirmCleanup:(NSString *)plan {
    NSAlert *alert = [[NSAlert alloc] init];
    alert.alertStyle = NSAlertStyleCritical;
    alert.messageText = @"Remove Runberth from the server?";
    alert.informativeText = @"Cleanup removes everything in this plan, including each runner's GitHub "
                            @"registration. It cannot be undone. Change the repositories' runs-on first.";
    NSScrollView *scroll = [[NSScrollView alloc] initWithFrame:NSMakeRect(0, 0, 460, 220)];
    scroll.hasVerticalScroller = YES;
    scroll.borderType = NSBezelBorder;
    NSTextView *text = [[NSTextView alloc] initWithFrame:scroll.contentView.bounds];
    text.editable = NO;
    text.font = [NSFont monospacedSystemFontOfSize:NSFont.smallSystemFontSize weight:NSFontWeightRegular];
    text.string = plan;
    text.autoresizingMask = NSViewWidthSizable;
    scroll.documentView = text;
    alert.accessoryView = scroll;
    NSButton *remove = [alert addButtonWithTitle:@"Remove Everything"];
    remove.hasDestructiveAction = YES;
    [alert addButtonWithTitle:@"Cancel"].keyEquivalent = @"\033";
    // Return must not confirm by accident.
    remove.keyEquivalent = @"";
    [alert beginSheetModalForWindow:self.window
                  completionHandler:^(NSModalResponse response) {
                    if (response == NSAlertFirstButtonReturn) {
                        setStatus(self.cleanupStatus, @"Removing Runberth from the server…", YES);
                        emit(@"cleanup-execute", @"");
                    } else {
                        self.cleanupButton.enabled = YES;
                        self.cleanupStatus.hidden = YES;
                    }
                  }];
}
- (void)status:(NSDictionary *)state {
    if (state[@"error"])
        for (NSTextField *label in self.errorLabels)
            setStatus(label, state[@"error"], NO);
    if (state[@"test"])
        setStatus(self.testStatus, state[@"test"], [state[@"test_ok"] boolValue]);
    if (state[@"cleanup_plan"])
        [self confirmCleanup:state[@"cleanup_plan"]];
    if (state[@"cleanup"]) {
        setStatus(self.cleanupStatus, state[@"cleanup"], [state[@"cleanup_ok"] boolValue]);
        self.cleanupButton.enabled = YES;
    }
    if (state[@"token_stored"])
        self.tokenState.stringValue = [state[@"token_stored"] boolValue] ? @"Stored in your keychain"
                                                                         : @"No token stored";
    if (state[@"token_message"]) {
        setStatus(self.tokenMessage, state[@"token_message"], [state[@"token_ok"] boolValue]);
        if ([state[@"token_ok"] boolValue])
            self.token.stringValue = @"";
    }
    if (state[@"login"]) {
        int login = [state[@"login"] intValue];
        self.loginBox.enabled = login >= 0;
        self.loginBox.state = login == 1 || login == 2 ? NSControlStateValueOn : NSControlStateValueOff;
        setStatus(self.loginNote,
                  login < 0   ? @"Available in the Runberth app on macOS 13 or later."
                  : login == 2 ? @"Approve Runberth in System Settings > General > Login Items."
                               : @"",
                  YES);
    }
}
- (NSString *)formJSON {
    NSDictionary *form = @{
        @"host" : self.host.stringValue,
        @"user" : self.user.stringValue,
        @"port" : self.port.stringValue,
        @"key" : self.key.stringValue,
        @"token_command" : self.tokenCommand.stringValue,
        @"interval" : self.interval.stringValue,
        @"notifications" : self.notifications.selectedItem.representedObject ?: @"all",
        @"check_updates" : @(self.updatesBox.state == NSControlStateValueOn),
    };
    NSData *json = [NSJSONSerialization dataWithJSONObject:form options:0 error:NULL];
    return [[NSString alloc] initWithData:json encoding:NSUTF8StringEncoding];
}
- (void)save {
    [self.saveTimer invalidate];
    self.saveTimer = nil;
    emit(@"settings-change", [self formJSON]);
}
- (void)controlTextDidChange:(NSNotification *)notification {
    [self.saveTimer invalidate];
    self.saveTimer = [NSTimer scheduledTimerWithTimeInterval:0.6
                                                     repeats:NO
                                                       block:^(NSTimer *timer) {
                                                         (void)timer;
                                                         [self save];
                                                       }];
}
- (void)changed:(id)sender {
    [self save];
    self.checkInterval.enabled = self.updatesBox.state == NSControlStateValueOn;
}
- (void)windowWillClose:(NSNotification *)notification {
    if (self.saveTimer)
        [self save];
}
- (void)chooseKey:(id)sender {
    NSOpenPanel *panel = [NSOpenPanel openPanel];
    panel.showsHiddenFiles = YES;
    panel.message = @"Choose the private key for the server";
    panel.directoryURL = [NSURL fileURLWithPath:[@"~/.ssh" stringByExpandingTildeInPath] isDirectory:YES];
    [panel beginSheetModalForWindow:self.window
                  completionHandler:^(NSModalResponse response) {
                    if (response != NSModalResponseOK)
                        return;
                    self.key.stringValue = [panel.URL.path stringByAbbreviatingWithTildeInPath];
                    [self save];
                  }];
}
- (void)saveToken:(id)sender {
    if (self.token.stringValue.length)
        emit(@"token-save", self.token.stringValue);
}
- (void)removeToken:(id)sender {
    emit(@"token-remove", @"");
}
- (void)createToken:(id)sender {
    [NSWorkspace.sharedWorkspace
        openURL:[NSURL URLWithString:@"https://github.com/settings/personal-access-tokens/new"]];
}
- (void)toggleLogin:(id)sender {
    emit(@"login-toggle", @"");
}
- (void)checkNow:(id)sender {
    emit(@"check-updates", @"");
}
- (void)openFile:(id)sender {
    [self save];
    emit(@"settings-file", @"");
}
@end

@implementation Tray
- (void)testConnection:(id)sender {
    // Test what the window shows, not an edit still waiting to be saved.
    [settings save];
    emit(@"settings-test", @"");
}
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
    // Accessory apps show no menu bar, but its key equivalents still work in
    // the Settings window: Command-W closes it, Command-V pastes a token.
    NSMenu *mainMenu = [[NSMenu alloc] init];
    NSMenu *appMenu = [[NSMenu alloc] initWithTitle:@"Runberth"];
    [appMenu addItemWithTitle:@"Close Window" action:@selector(performClose:) keyEquivalent:@"w"];
    [mainMenu addItemWithTitle:@"Runberth" action:nil keyEquivalent:@""].submenu = appMenu;
    NSMenu *editMenu = [[NSMenu alloc] initWithTitle:@"Edit"];
    [editMenu addItemWithTitle:@"Undo" action:@selector(undo:) keyEquivalent:@"z"];
    [editMenu addItemWithTitle:@"Redo" action:@selector(redo:) keyEquivalent:@"Z"];
    [editMenu addItem:[NSMenuItem separatorItem]];
    [editMenu addItemWithTitle:@"Cut" action:@selector(cut:) keyEquivalent:@"x"];
    [editMenu addItemWithTitle:@"Copy" action:@selector(copy:) keyEquivalent:@"c"];
    [editMenu addItemWithTitle:@"Paste" action:@selector(paste:) keyEquivalent:@"v"];
    [editMenu addItemWithTitle:@"Select All" action:@selector(selectAll:) keyEquivalent:@"a"];
    [mainMenu addItemWithTitle:@"Edit" action:nil keyEquivalent:@""].submenu = editMenu;
    NSApp.mainMenu = mainMenu;
    self.status = [[NSStatusBar systemStatusBar] statusItemWithLength:NSVariableStatusItemLength];
    self.status.autosaveName = @"dev.runberth.app.status";
    self.status.button.image = trayIcon(@"idle", 0);
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
// Opening the app again from Finder or Spotlight while it runs shows Settings.
- (BOOL)applicationShouldHandleReopen:(NSApplication *)sender hasVisibleWindows:(BOOL)visible {
    emit(@"open-settings", @"");
    return NO;
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
- (void)showIcon:(NSString *)state {
    if ([state isEqualToString:self.iconState])
        return;
    self.iconState = state;
    [self.spinTimer invalidate];
    self.spinTimer = nil;
    self.status.button.image = trayIcon(state, 0);
    if (![state isEqualToString:@"running"])
        return;
    // About 1.2 seconds a turn; the timer stops when the jobs do.
    self.spin = 0;
    self.spinTimer = [NSTimer timerWithTimeInterval:0.1
                                            repeats:YES
                                              block:^(NSTimer *timer) {
                                                (void)timer;
                                                self.spin = fmod(self.spin + 30, 360);
                                                self.status.button.image = trayIcon(@"running", self.spin);
                                              }];
    // Common modes keep it turning while the menu is open.
    [[NSRunLoop mainRunLoop] addTimer:self.spinTimer forMode:NSRunLoopCommonModes];
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
    [self showIcon:spec[@"icon"]];
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

static NSDictionary *parse(NSString *json) {
    id value = [NSJSONSerialization JSONObjectWithData:[json dataUsingEncoding:NSUTF8StringEncoding]
                                               options:0
                                                 error:NULL];
    return [value isKindOfClass:NSDictionary.class] ? value : nil;
}

void native_show_settings(const char *window_json) {
    NSString *json = @(window_json);
    dispatch_async(dispatch_get_main_queue(), ^{
      if (!settings)
          settings = [[Settings alloc] init];
      [settings show:parse(json)];
    });
}

void native_settings_status(const char *status_json) {
    NSString *json = @(status_json);
    dispatch_async(dispatch_get_main_queue(), ^{
      [settings status:parse(json)];
    });
}

void native_set_update_checks(int enabled) {
    [[tray.updaterController valueForKey:@"updater"] setValue:@(enabled != 0)
                                                       forKey:@"automaticallyChecksForUpdates"];
}

int native_has_updater(void) {
    return tray.updaterController != nil;
}

void native_check_updates(void) {
    [tray.updaterController checkForUpdates:nil];
}
