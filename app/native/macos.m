#include "bridge.h"
#include "tray_icons.h"
#import <AppKit/AppKit.h>
#import <QuartzCore/QuartzCore.h>
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
 * check when the latest runs passed, a red "!" when one failed, an amber
 * warning triangle when job results need a working GitHub token, and an orange
 * spinner, turned `angle` degrees, while jobs run. "idle" has no badge. */
static NSImage *trayIcon(NSString *state, CGFloat angle) {
    static NSImage *glyph, *cut, *cutAttention, *success, *failed, *offline, *attention, *running;
    if (!glyph) {
        glyph = layer(tray_glyph, sizeof tray_glyph);
        cut = layer(tray_glyph_cut, sizeof tray_glyph_cut);
        success = layer(tray_success, sizeof tray_success);
        failed = layer(tray_failed, sizeof tray_failed);
        offline = layer(tray_offline, sizeof tray_offline);
        cutAttention = layer(tray_glyph_cut_attention, sizeof tray_glyph_cut_attention);
        attention = layer(tray_attention, sizeof tray_attention);
        running = layer(tray_running, sizeof tray_running);
    }
    NSDictionary *badges = @{
        @"success" : success,
        @"failed" : failed,
        @"offline" : offline,
        @"attention" : attention,
        @"running" : running
    };
    NSImage *badge = badges[state];
    // The triangle needs its own cut-out; the round badges share one.
    NSImage *shape = !badge ? glyph : [state isEqualToString:@"attention"] ? cutAttention : cut;
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
                                  // The badge's centre on the 24-unit grid, (4.9, 4.9), at 20
                                  // points: x 4.9 * 20/24, y (24 - 4.9) * 20/24 from the bottom.
                                  NSAffineTransform *turn = [NSAffineTransform transform];
                                  [turn translateXBy:4.083 yBy:15.917];
                                  [turn rotateByDegrees:-angle];
                                  [turn translateXBy:-4.083 yBy:-15.917];
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
static NSString *const ProjectURL = @"https://github.com/vantalter/runberth";

static NSImage *symbol(NSString *name, NSString *description) {
    return [NSImage imageWithSystemSymbolName:name accessibilityDescription:description];
}


/* A tab: its content, centred. */
static NSTabViewItem *pane(NSString *title, NSImage *image, NSView *content) {
    NSViewController *controller = [[NSViewController alloc] init];
    controller.title = title;
    NSView *view = [[NSView alloc] init];
    content.translatesAutoresizingMaskIntoConstraints = NO;
    [view addSubview:content];
    [NSLayoutConstraint activateConstraints:@[
        [content.topAnchor constraintEqualToAnchor:view.topAnchor constant:20],
        [content.leadingAnchor constraintGreaterThanOrEqualToAnchor:view.leadingAnchor constant:30],
        [content.centerXAnchor constraintEqualToAnchor:view.centerXAnchor],
        [content.bottomAnchor constraintEqualToAnchor:view.bottomAnchor constant:-20],
        [view.widthAnchor constraintEqualToConstant:540],
    ]];
    controller.view = view;
    [view layoutSubtreeIfNeeded];
    controller.preferredContentSize = view.fittingSize;
    NSTabViewItem *item = [NSTabViewItem tabViewItemWithViewController:controller];
    item.image = image;
    return item;
}

/* A status line that always takes one line, empty or not, so a message
 * never changes the window's size; a long one is cut short, and the full
 * text shows on hover. */
static NSTextField *statusLine(CGFloat width) {
    NSTextField *label = [NSTextField labelWithString:@" "];
    label.font = [NSFont systemFontOfSize:NSFont.smallSystemFontSize];
    label.textColor = NSColor.secondaryLabelColor;
    label.lineBreakMode = NSLineBreakByTruncatingTail;
    label.maximumNumberOfLines = 1;
    label.usesSingleLineMode = YES;
    [label.widthAnchor constraintEqualToConstant:width].active = YES;
    // One line high whatever it is given, so no text can resize the window.
    [label.heightAnchor constraintEqualToConstant:ceil(label.font.ascender - label.font.descender +
                                                       label.font.leading) + 2]
        .active = YES;
    return label;
}

/* The Settings window's signal colours: red for destructive buttons and
 * the warning icon, and a darker red that reads as text for errors. The
 * Activity window uses the system's semantic colours, but for its green. */
static NSColor *appRed(void) {
    return [NSColor colorWithSRGBRed:254 / 255.0 green:142 / 255.0 blue:136 / 255.0 alpha:1];
}
static NSColor *appError(void) {
    return [NSColor colorWithSRGBRed:195 / 255.0 green:0 blue:0 alpha:1];
}
/* Green for work that ended well and output still arriving. */
static NSColor *successGreen(void) {
    return [NSColor colorWithSRGBRed:102 / 255.0 green:211 / 255.0 blue:87 / 255.0 alpha:1];
}

static void setStatus(NSTextField *label, NSString *text, BOOL ok) {
    // A status is one line: breaks inside it, such as a progress counter's
    // carriage returns, become spaces.
    NSArray<NSString *> *parts =
        [text componentsSeparatedByCharactersInSet:NSCharacterSet.newlineCharacterSet];
    text = [[parts filteredArrayUsingPredicate:[NSPredicate predicateWithFormat:@"length > 0"]]
        componentsJoinedByString:@" "];
    // A space keeps the line's height while it is empty.
    label.stringValue = text.length ? text : @" ";
    label.toolTip = text.length ? text : nil;
    label.textColor = ok ? NSColor.secondaryLabelColor : appError();
}

/* The Activity window: an entry for each piece of work, running or
 * recently ended, newest first. Each shows what it does and on which
 * server, how long it has taken, its step and a bar when the work numbers
 * its steps, and under Operation details what it printed. */

/* "00:24", or "1:02:03" past an hour. */
static NSString *elapsedClock(long seconds) {
    if (seconds < 0)
        seconds = 0;
    if (seconds >= 3600)
        return [NSString stringWithFormat:@"%ld:%02ld:%02ld", seconds / 3600, seconds / 60 % 60, seconds % 60];
    return [NSString stringWithFormat:@"%02ld:%02ld", seconds / 60, seconds % 60];
}

/* A rounded box, drawn in the system's colours in light and dark mode. */
static NSBox *roundedBox(NSColor *fill, CGFloat radius) {
    NSBox *box = [[NSBox alloc] init];
    box.boxType = NSBoxCustom;
    box.titlePosition = NSNoTitle;
    box.fillColor = fill;
    box.borderColor = NSColor.clearColor;
    box.borderWidth = 0;
    box.cornerRadius = radius;
    box.contentViewMargins = NSZeroSize;
    return box;
}

static NSBox *ruleLine(void) {
    NSBox *line = [[NSBox alloc] init];
    line.boxType = NSBoxSeparator;
    return line;
}

/* Advice in the output, as `Recommendation: …`: amber, darker in light mode
 * so it reads on the log's grey. */
static NSColor *adviceColor(void) {
    return [NSColor colorWithName:nil
                  dynamicProvider:^NSColor *(NSAppearance *appearance) {
                    BOOL dark = [appearance bestMatchFromAppearancesWithNames:@[
                        NSAppearanceNameAqua, NSAppearanceNameDarkAqua
                    ]] == NSAppearanceNameDarkAqua;
                    return dark ? NSColor.systemOrangeColor
                                : [NSColor colorWithSRGBRed:160 / 255.0 green:100 / 255.0 blue:20 / 255.0 alpha:1];
                  }];
}

/* A thin turning ring, the spinner of work under way. It holds still with
 * Reduce Motion on. */
@interface RingSpinner : NSView
@property CAShapeLayer *ring;
@property(nonatomic) BOOL spinning;
@end
@implementation RingSpinner
- (instancetype)initWithFrame:(NSRect)frame {
    if ((self = [super initWithFrame:frame])) {
        self.wantsLayer = YES;
        self.ring = [CAShapeLayer layer];
        self.ring.fillColor = NSColor.clearColor.CGColor;
        self.ring.lineWidth = 2.5;
        self.ring.lineCap = kCALineCapRound;
        self.ring.strokeStart = 0;
        self.ring.strokeEnd = 0.8;
        [self.layer addSublayer:self.ring];
    }
    return self;
}
- (void)layout {
    [super layout];
    self.ring.frame = self.bounds;
    CGFloat inset = self.ring.lineWidth / 2 + 1;
    CGPathRef path = CGPathCreateWithEllipseInRect(CGRectInset(self.bounds, inset, inset), NULL);
    self.ring.path = path;
    CGPathRelease(path);
    [self updateColor];
}
- (void)updateColor {
    [self.effectiveAppearance performAsCurrentDrawingAppearance:^{
      self.ring.strokeColor = NSColor.secondaryLabelColor.CGColor;
    }];
}
- (void)viewDidChangeEffectiveAppearance {
    [super viewDidChangeEffectiveAppearance];
    [self updateColor];
}
- (void)setSpinning:(BOOL)spinning {
    if (_spinning == spinning && (!spinning || [self.ring animationForKey:@"turn"]))
        return;
    _spinning = spinning;
    self.hidden = !spinning;
    [self.ring removeAnimationForKey:@"turn"];
    if (!spinning || NSWorkspace.sharedWorkspace.accessibilityDisplayShouldReduceMotion)
        return;
    CABasicAnimation *turn = [CABasicAnimation animationWithKeyPath:@"transform.rotation.z"];
    turn.fromValue = @0;
    turn.toValue = @(-2 * M_PI);
    turn.duration = 1;
    turn.repeatCount = HUGE_VALF;
    [self.ring addAnimation:turn forKey:@"turn"];
}
@end

static NSTextField *textLabel(CGFloat size, NSFontWeight weight, NSColor *color) {
    NSTextField *text = [NSTextField labelWithString:@""];
    text.font = [NSFont systemFontOfSize:size weight:weight];
    text.textColor = color;
    text.lineBreakMode = NSLineBreakByTruncatingTail;
    [text setContentCompressionResistancePriority:NSLayoutPriorityDefaultLow
                                   forOrientation:NSLayoutConstraintOrientationHorizontal];
    return text;
}

static NSStackView *stackOf(NSArray<NSView *> *views, NSUserInterfaceLayoutOrientation orientation,
                          NSLayoutAttribute alignment, CGFloat spacing) {
    NSStackView *view = [NSStackView stackViewWithViews:views];
    view.orientation = orientation;
    view.alignment = alignment;
    view.spacing = spacing;
    return view;
}

static NSView *flexibleGap(void) {
    NSView *gap = [[NSView alloc] init];
    [gap setContentHuggingPriority:1 forOrientation:NSLayoutConstraintOrientationHorizontal];
    return gap;
}

/* The window's padding around its entries, and its default and least
 * content widths; entries take the rest of the width as it is resized. */
static const CGFloat ActivityPadding = 20;
static const CGFloat ActivityWidth = 560;
static const CGFloat ActivityMinimumWidth = 440;

/* The log's text size, which ⌘+, ⌘− and ⌘0 change, kept across launches. */
static NSString *const LogFontSizeKey = @"ActivityLogFontSize";
static CGFloat logFontSize(void) {
    CGFloat size = [NSUserDefaults.standardUserDefaults doubleForKey:LogFontSizeKey];
    return size >= 10 && size <= 18 ? size : 11;
}

/* Titles without "on the server" or "from the server": the line beneath
 * names the server. */
static NSString *shortTitle(NSString *title) {
    for (NSString *tail in @[ @" on the server", @" from the server" ])
        if ([title hasSuffix:tail])
            return [title substringToIndex:title.length - tail.length];
    return title;
}

@interface ActivityEntry : NSObject
@property NSStackView *view;
@property RingSpinner *spinner;
@property NSImageView *icon;
@property NSTextField *title, *target, *timer, *stepName, *stepCount, *status;
@property NSProgressIndicator *bar;
@property NSView *progress, *dot;
@property NSBox *rule;
@property NSButton *disclosure, *detailsButton, *clipboardButton;
@property NSStackView *detailsRow, *footer;
@property NSBox *log;
@property NSScrollView *outputScroll;
@property NSTextView *output;
/* What the output shows now, so an unchanged update keeps the selection. */
@property NSString *shownOutput;
/* Whether the log keeps its newest line in view: on until scrolled up. */
@property BOOL following;
/* While the log scrolls itself, which is no reason to stop following. */
@property BOOL scrollingToEnd;
/* When it started, ended, and last printed, in seconds since the epoch;
 * ended is nil while it runs, and the timer ticks on its own. */
@property NSNumber *started, *ended, *heard;
/* How the work ended, for the footer's dot. */
@property BOOL succeeded;
/* The activity it shows, to restyle the log when its text size changes. */
@property NSDictionary *activity;
/* Called when the entry's height changes, so the window fits it. */
@property(copy) void (^resized)(void);
@end

@implementation ActivityEntry
- (instancetype)init {
    if ((self = [super init])) {
        // Head: the ring or the result's symbol, what it does and where,
        // and the time on the right.
        self.spinner = [[RingSpinner alloc] initWithFrame:NSMakeRect(0, 0, 20, 20)];
        self.icon = [[NSImageView alloc] init];
        self.icon.symbolConfiguration = [NSImageSymbolConfiguration configurationWithPointSize:18
                                                                                       weight:NSFontWeightRegular];
        NSView *slot = [[NSView alloc] init];
        for (NSView *part in @[ self.spinner, self.icon ]) {
            part.translatesAutoresizingMaskIntoConstraints = NO;
            [slot addSubview:part];
            [NSLayoutConstraint activateConstraints:@[
                [part.centerXAnchor constraintEqualToAnchor:slot.centerXAnchor],
                [part.centerYAnchor constraintEqualToAnchor:slot.centerYAnchor],
            ]];
        }
        [NSLayoutConstraint activateConstraints:@[
            [self.spinner.widthAnchor constraintEqualToConstant:20],
            [self.spinner.heightAnchor constraintEqualToConstant:20],
            [slot.widthAnchor constraintEqualToConstant:22],
            [slot.heightAnchor constraintEqualToConstant:22],
        ]];
        self.title = textLabel(17, NSFontWeightSemibold, NSColor.labelColor);
        self.target = textLabel(12, NSFontWeightRegular, NSColor.secondaryLabelColor);
        self.timer = textLabel(13, NSFontWeightMedium, NSColor.secondaryLabelColor);
        self.timer.font = [NSFont monospacedDigitSystemFontOfSize:13 weight:NSFontWeightMedium];
        // A fixed place at the right, room for an hour's "1:02:03", so a
        // new digit never moves it or the title.
        self.timer.alignment = NSTextAlignmentRight;
        [self.timer.widthAnchor constraintEqualToConstant:64].active = YES;
        NSStackView *texts = stackOf(@[ self.title, self.target ], NSUserInterfaceLayoutOrientationVertical,
                                   NSLayoutAttributeLeading, 4);
        NSStackView *head = stackOf(@[ slot, texts, flexibleGap(), self.timer ],
                                  NSUserInterfaceLayoutOrientationHorizontal, NSLayoutAttributeTop, 12);
        // The ring and the time beside the title's line.
        [slot.centerYAnchor constraintEqualToAnchor:self.title.centerYAnchor].active = YES;
        [self.timer.firstBaselineAnchor constraintEqualToAnchor:self.title.firstBaselineAnchor].active = YES;

        // Steps, when the work numbers them: the bar of steps finished, the
        // current one's name, and which of how many it is.
        self.bar = [[NSProgressIndicator alloc] init];
        self.bar.style = NSProgressIndicatorStyleBar;
        self.bar.indeterminate = NO;
        self.bar.minValue = 0;
        self.bar.maxValue = 1;
        self.stepName = textLabel(12, NSFontWeightRegular, NSColor.secondaryLabelColor);
        self.stepCount = textLabel(12, NSFontWeightMedium, NSColor.secondaryLabelColor);
        // Fixed-width digits, so "Step 9 of 12" never shifts as it counts.
        self.stepCount.font = [NSFont monospacedDigitSystemFontOfSize:12 weight:NSFontWeightMedium];
        [self.stepCount setContentCompressionResistancePriority:NSLayoutPriorityRequired
                                                 forOrientation:NSLayoutConstraintOrientationHorizontal];
        NSStackView *stepRow = stackOf(@[ self.stepName, flexibleGap(), self.stepCount ],
                                     NSUserInterfaceLayoutOrientationHorizontal, NSLayoutAttributeFirstBaseline, 12);
        self.progress = stackOf(@[ self.bar, stepRow ], NSUserInterfaceLayoutOrientationVertical,
                              NSLayoutAttributeLeading, 6);
        for (NSView *part in @[ self.bar, stepRow ])
            [part.widthAnchor constraintEqualToAnchor:self.progress.widthAnchor].active = YES;

        self.rule = ruleLine();

        // Operation details: the triangle and its words open the log
        // together, with Copy logs on the right.
        self.disclosure = [[NSButton alloc] init];
        self.disclosure.bezelStyle = NSBezelStyleDisclosure;
        [self.disclosure setButtonType:NSButtonTypePushOnPushOff];
        self.disclosure.title = @"";
        self.disclosure.target = self;
        self.disclosure.action = @selector(toggle:);
        self.detailsButton = [NSButton buttonWithTitle:@"Operation details" target:self action:@selector(toggleByTitle:)];
        self.detailsButton.bordered = NO;
        self.detailsButton.font = [NSFont systemFontOfSize:13 weight:NSFontWeightMedium];
        self.clipboardButton = [NSButton buttonWithTitle:@"Copy logs"
                                                   image:[NSImage imageWithSystemSymbolName:@"doc.on.doc"
                                                                   accessibilityDescription:nil]
                                                  target:self
                                                  action:@selector(copyOutput:)];
        self.clipboardButton.imagePosition = NSImageLeading;
        self.clipboardButton.controlSize = NSControlSizeSmall;
        self.clipboardButton.font = [NSFont systemFontOfSize:12];
        self.clipboardButton.toolTip = @"Copies the selected text, or all of the output";
        self.clipboardButton.hidden = YES;
        self.detailsRow = stackOf(@[ self.disclosure, self.detailsButton, flexibleGap(), self.clipboardButton ],
                                NSUserInterfaceLayoutOrientationHorizontal, NSLayoutAttributeCenterY, 4);

        // The log: what the work printed, step lines as headings.
        self.outputScroll = [NSTextView scrollableTextView];
        self.outputScroll.drawsBackground = NO;
        self.outputScroll.borderType = NSNoBorder;
        self.output = self.outputScroll.documentView;
        self.output.editable = NO;
        self.output.selectable = YES;
        self.output.drawsBackground = NO;
        self.output.textContainerInset = NSZeroSize;
        self.outputScroll.contentView.postsBoundsChangedNotifications = YES;
        [NSNotificationCenter.defaultCenter addObserver:self
                                               selector:@selector(outputScrolled:)
                                                   name:NSViewBoundsDidChangeNotification
                                                 object:self.outputScroll.contentView];
        self.log = roundedBox(NSColor.textBackgroundColor, 8);
        self.outputScroll.translatesAutoresizingMaskIntoConstraints = NO;
        [self.log.contentView addSubview:self.outputScroll];
        [NSLayoutConstraint activateConstraints:@[
            [self.outputScroll.topAnchor constraintEqualToAnchor:self.log.contentView.topAnchor constant:10],
            [self.outputScroll.bottomAnchor constraintEqualToAnchor:self.log.contentView.bottomAnchor constant:-10],
            [self.outputScroll.leadingAnchor constraintEqualToAnchor:self.log.contentView.leadingAnchor constant:12],
            [self.outputScroll.trailingAnchor constraintEqualToAnchor:self.log.contentView.trailingAnchor constant:-12],
            [self.outputScroll.heightAnchor constraintGreaterThanOrEqualToConstant:120],
        ]];
        // 240 high to start; resizing the window grows or shrinks it.
        NSLayoutConstraint *preferred = [self.outputScroll.heightAnchor constraintEqualToConstant:240];
        preferred.priority = NSLayoutPriorityDefaultLow;
        preferred.active = YES;
        [self.log setContentHuggingPriority:1 forOrientation:NSLayoutConstraintOrientationVertical];
        self.log.hidden = YES;

        // Under the log: whether lines still arrive.
        self.dot = [[NSView alloc] init];
        self.dot.wantsLayer = YES;
        self.dot.layer.cornerRadius = 3;
        [NSLayoutConstraint activateConstraints:@[
            [self.dot.widthAnchor constraintEqualToConstant:6],
            [self.dot.heightAnchor constraintEqualToConstant:6],
        ]];
        self.status = textLabel(11, NSFontWeightRegular, NSColor.secondaryLabelColor);
        self.footer = stackOf(@[ self.dot, self.status ],
                            NSUserInterfaceLayoutOrientationHorizontal, NSLayoutAttributeCenterY, 6);
        self.footer.hidden = YES;
        self.following = YES;

        self.view = stackOf(@[ head, self.progress, self.rule, self.detailsRow, self.log, self.footer ],
                          NSUserInterfaceLayoutOrientationVertical, NSLayoutAttributeLeading, 16);
        self.view.distribution = NSStackViewDistributionFill;
        [self.view setCustomSpacing:16 afterView:head];
        [self.view setCustomSpacing:16 afterView:self.progress];
        [self.view setCustomSpacing:8 afterView:self.detailsRow];
        [self.view setCustomSpacing:10 afterView:self.log];
        for (NSView *part in @[ head, self.progress, self.rule, self.detailsRow, self.log, self.footer ])
            [part.widthAnchor constraintEqualToAnchor:self.view.widthAnchor].active = YES;
        // Only the log takes height the window is given.
        for (NSView *part in @[ head, self.progress, self.rule, self.detailsRow, self.footer ])
            [part setContentHuggingPriority:NSLayoutPriorityDefaultHigh
                             forOrientation:NSLayoutConstraintOrientationVertical];
    }
    return self;
}
- (void)dealloc {
    [NSNotificationCenter.defaultCenter removeObserver:self];
}
/* Whether the output shows its last line. */
- (BOOL)atLatest {
    NSClipView *clip = self.outputScroll.contentView;
    return NSMaxY(clip.bounds) >= NSHeight(self.output.frame) - 4;
}
/* Scrolling up to read stops following; scrolling back down resumes. */
- (void)outputScrolled:(NSNotification *)notification {
    if (!self.scrollingToEnd)
        self.following = [self atLatest];
}
/* The newest line in view, once the text is laid out: new text is laid
 * out lazily, and scrolling before that would stop at the old end. */
- (void)scrollToEnd {
    self.scrollingToEnd = YES;
    [self.output.layoutManager ensureLayoutForTextContainer:self.output.textContainer];
    [self.output scrollRangeToVisible:NSMakeRange(self.output.string.length, 0)];
    self.scrollingToEnd = NO;
}
- (void)copyOutput:(id)sender {
    NSRange selected = self.output.selectedRange;
    NSString *text = selected.length ? [self.output.string substringWithRange:selected] : self.output.string;
    [NSPasteboard.generalPasteboard clearContents];
    [NSPasteboard.generalPasteboard setString:text forType:NSPasteboardTypeString];
    self.clipboardButton.title = @"Copied";
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, (int64_t)(1.5 * NSEC_PER_SEC)), dispatch_get_main_queue(), ^{
      self.clipboardButton.title = @"Copy logs";
    });
}
- (void)toggle:(id)sender {
    BOOL open = self.disclosure.state == NSControlStateValueOn;
    self.log.hidden = !open;
    self.footer.hidden = !open;
    self.clipboardButton.hidden = !open;
    if (open && self.following) {
        [self.view layoutSubtreeIfNeeded];
        [self scrollToEnd];
    }
    if (self.resized)
        self.resized();
}
- (void)toggleByTitle:(id)sender {
    self.disclosure.state = self.disclosure.state == NSControlStateValueOn ? NSControlStateValueOff : NSControlStateValueOn;
    [self toggle:sender];
}
/* Each second: the time taken, up to now while it runs, then fixed; and
 * whether lines still arrive. */
- (void)tick {
    long now = (long)NSDate.date.timeIntervalSince1970;
    long end = self.ended ? self.ended.longValue : now;
    self.timer.stringValue = elapsedClock(end - self.started.longValue);
    NSColor *dot = NSColor.tertiaryLabelColor;
    NSString *status;
    if (self.ended) {
        status = self.succeeded ? @"Completed" : @"Failed";
        dot = self.succeeded ? successGreen() : NSColor.systemRedColor;
    } else if (!self.heard) {
        status = @"Waiting for output";
    } else if (now - self.heard.longValue < 5) {
        status = @"Receiving output";
        dot = successGreen();
    } else {
        // Quiet is not failure: a long package install prints little.
        status = [NSString stringWithFormat:@"No new output for %@", elapsedClock(now - self.heard.longValue)];
    }
    self.status.stringValue = status;
    [self.view.effectiveAppearance performAsCurrentDrawingAppearance:^{
      self.dot.layer.backgroundColor = dot.CGColor;
    }];
}
/* The log in the system's monospaced font, which keeps the server's
 * columns, tables, and indentation as printed: step lines, as `[4/7]
 * Checking the server`, are headings, earlier ones dimmed and the current
 * one medium, with what each printed indented beneath; advice and warnings
 * are amber, errors red, and once the work ends its result follows in green
 * or red. */
- (NSAttributedString *)styledOutput:(NSString *)tail running:(BOOL)running ending:(NSString *)ending ok:(BOOL)ok {
    CGFloat size = logFontSize();
    NSMutableParagraphStyle *plain = [[NSMutableParagraphStyle alloc] init];
    plain.lineSpacing = 2;
    NSMutableParagraphStyle *indented = [plain mutableCopy];
    indented.firstLineHeadIndent = 16;
    indented.headIndent = 16;
    NSArray<NSString *> *lines = tail.length ? [tail componentsSeparatedByString:@"\n"] : @[];
    NSUInteger lastStep = NSNotFound;
    for (NSUInteger index = 0; index < lines.count; index++)
        if ([lines[index] hasPrefix:@"["] && [lines[index] rangeOfString:@"] "].location != NSNotFound &&
            [lines[index] rangeOfString:@"/"].location < [lines[index] rangeOfString:@"] "].location)
            lastStep = index;
    NSMutableAttributedString *text = [[NSMutableAttributedString alloc] init];
    BOOL underStep = NO;
    for (NSUInteger index = 0; index < lines.count; index++) {
        NSString *line = lines[index];
        NSRange close = [line rangeOfString:@"] "];
        BOOL step = [line hasPrefix:@"["] && close.location != NSNotFound &&
                    [line rangeOfString:@"/"].location < close.location;
        NSColor *color = NSColor.labelColor;
        NSFontWeight weight = NSFontWeightRegular;
        NSString *lower = line.lowercaseString;
        if (step) {
            underStep = YES;
            if (index == lastStep && running)
                weight = NSFontWeightMedium;
            else
                color = NSColor.secondaryLabelColor;
        } else if ([lower hasPrefix:@"warning"]) {
            color = adviceColor();
            weight = NSFontWeightMedium;
        } else if ([line hasPrefix:@"Recommendation"]) {
            color = adviceColor();
        } else if ([lower hasPrefix:@"error"] || [line hasPrefix:@"✗"]) {
            color = NSColor.systemRedColor;
        }
        NSString *piece = [NSString stringWithFormat:@"%@%@", text.length ? @"\n" : @"", line];
        [text appendAttributedString:[[NSAttributedString alloc]
                                         initWithString:piece
                                             attributes:@{
                                                 NSFontAttributeName : [NSFont monospacedSystemFontOfSize:size weight:weight],
                                                 NSForegroundColorAttributeName : color,
                                                 NSParagraphStyleAttributeName : step || !underStep ? plain : indented,
                                             }]];
    }
    if (ending.length) {
        NSString *piece = [NSString stringWithFormat:@"%@%@", text.length ? @"\n\n" : @"", ending];
        [text appendAttributedString:[[NSAttributedString alloc]
                                         initWithString:piece
                                             attributes:@{
                                                 NSFontAttributeName : [NSFont monospacedSystemFontOfSize:size weight:NSFontWeightSemibold],
                                                 NSForegroundColorAttributeName : ok ? successGreen() : NSColor.systemRedColor,
                                                 NSParagraphStyleAttributeName : plain,
                                             }]];
    }
    return text;
}
/* The log again in its current text size. */
- (void)restyle {
    self.shownOutput = nil;
    if (self.activity)
        [self show:self.activity];
}
- (void)show:(NSDictionary *)activity {
    self.activity = activity;
    BOOL running = [activity[@"running"] boolValue];
    BOOL ok = [activity[@"ok"] boolValue];
    self.succeeded = ok;
    NSArray *progress = [activity[@"progress"] isKindOfClass:NSArray.class] ? activity[@"progress"] : nil;

    // Running: what it does. Ended: how, with why it failed beneath, from
    // a result such as "Couldn't connect aerofoil: GitHub rejected it".
    NSString *title = activity[@"title"] ?: @"", *reason = @"";
    if (!running) {
        NSString *result = activity[@"result"] ?: @"";
        NSRange colon = [result rangeOfString:@": "];
        if (colon.location != NSNotFound) {
            title = [result substringToIndex:colon.location];
            reason = [result substringFromIndex:NSMaxRange(colon)];
        } else if (result.length) {
            title = result;
        }
    }
    title = shortTitle(title);
    self.title.stringValue = title;
    self.title.toolTip = title;
    NSString *target = activity[@"target"] ?: @"";
    self.target.stringValue = target;
    self.target.hidden = target.length == 0;
    self.started = activity[@"started"];
    self.ended = [activity[@"finished"] isKindOfClass:NSNumber.class] ? activity[@"finished"] : nil;
    self.heard = [activity[@"heard"] isKindOfClass:NSNumber.class] ? activity[@"heard"] : nil;
    [self tick];

    self.spinner.spinning = running;
    self.icon.hidden = running;
    if (!running) {
        self.icon.image = [NSImage imageWithSystemSymbolName:ok ? @"checkmark.circle.fill" : @"xmark.circle.fill"
                                    accessibilityDescription:ok ? @"Done" : @"Failed"];
        self.icon.contentTintColor = ok ? successGreen() : NSColor.systemRedColor;
    }

    // The steps finished fill the bar: on the third of seven, two sevenths,
    // so it is never full before the work reports it is done. Work that
    // numbers no steps shows only its title; a failure, its reason.
    BOOL stepped = running && progress;
    self.bar.hidden = !stepped;
    self.stepCount.hidden = !stepped;
    if (stepped) {
        self.bar.doubleValue = ([progress[0] doubleValue] - 1) / [progress[1] doubleValue];
        NSString *step = activity[@"step"] ?: @"";
        self.stepName.stringValue = step.length ? [step stringByAppendingString:@"…"] : @"";
        self.stepName.textColor = NSColor.secondaryLabelColor;
        self.stepCount.stringValue = [NSString stringWithFormat:@"Step %@ of %@", progress[0], progress[1]];
    } else {
        self.stepName.stringValue = reason;
        self.stepName.textColor = NSColor.systemRedColor;
    }
    self.stepName.toolTip = self.stepName.stringValue.length ? self.stepName.stringValue : nil;
    self.progress.hidden = !stepped && reason.length == 0;

    // The log, kept at its newest line unless scrolled back to read.
    NSString *tail = activity[@"tail"] ?: @"";
    NSString *ending = running ? @"" : [NSString stringWithFormat:@"%@ %@ · took %@", ok ? @"✓" : @"✗",
                                                                   activity[@"result"] ?: @"", self.timer.stringValue];
    NSString *shown = [NSString stringWithFormat:@"%@\n%@\n%@", tail, ending, progress ?: @""];
    if (![self.shownOutput isEqualToString:shown]) {
        // Replacing the text moves the view; whether to follow is decided
        // before that.
        BOOL following = self.following;
        NSPoint reading = self.outputScroll.contentView.bounds.origin;
        self.scrollingToEnd = YES;
        [self.output.textStorage setAttributedString:[self styledOutput:tail running:running ending:ending ok:ok]];
        self.shownOutput = shown;
        if (following) {
            [self scrollToEnd];
        } else {
            // Scrolled up to read: the view stays on what was being read.
            [self.output.layoutManager ensureLayoutForTextContainer:self.output.textContainer];
            [self.outputScroll.contentView scrollToPoint:reading];
            [self.outputScroll reflectScrolledClipView:self.outputScroll.contentView];
        }
        self.scrollingToEnd = NO;
    }

    // Nothing to open until the work prints a line.
    BOOL detailed = tail.length > 0;
    self.rule.hidden = !detailed;
    self.detailsRow.hidden = !detailed;
    if (!detailed) {
        self.log.hidden = YES;
        self.footer.hidden = YES;
    }
}
@end

@interface ActivityWindow : NSObject <NSWindowDelegate>
@property NSWindow *window;
@property NSStackView *list;
@property NSTextField *empty;
@property NSMutableDictionary<NSNumber *, ActivityEntry *> *entries;
@property NSArray<NSDictionary *> *activities;
/* Work Settings just started, by its key: the window opens when it shows. */
@property NSString *awaited;
/* Ticks the running entries' timers each second while the window shows. */
@property NSTimer *clock;
/* The height the entries asked for last; the window refits only when it
 * changes, so a height the user dragged to stays. */
@property CGFloat fittedHeight;
/* ⌘+, ⌘−, and ⌘0 for the log's text size while the window is key. */
@property id keyMonitor;
@end

@implementation ActivityWindow
- (void)build {
    self.entries = [NSMutableDictionary dictionary];
    self.empty = [NSTextField labelWithString:@"No work in the last few minutes."];
    self.empty.textColor = NSColor.secondaryLabelColor;
    self.list = stackOf(@[ self.empty ], NSUserInterfaceLayoutOrientationVertical, NSLayoutAttributeCenterX, 16);
    self.list.distribution = NSStackViewDistributionFill;
    self.list.edgeInsets =
        NSEdgeInsetsMake(ActivityPadding, ActivityPadding, ActivityPadding, ActivityPadding);
    [self.list.widthAnchor constraintGreaterThanOrEqualToConstant:ActivityMinimumWidth].active = YES;
    NSView *content = [[NSView alloc] init];
    self.list.translatesAutoresizingMaskIntoConstraints = NO;
    [content addSubview:self.list];
    // `fit` sizes the window to the entries, growing as details open.
    [NSLayoutConstraint activateConstraints:@[
        [self.list.topAnchor constraintEqualToAnchor:content.topAnchor],
        [self.list.bottomAnchor constraintEqualToAnchor:content.bottomAnchor],
        [self.list.leadingAnchor constraintEqualToAnchor:content.leadingAnchor],
        [self.list.trailingAnchor constraintEqualToAnchor:content.trailingAnchor],
    ]];
    self.window = [[NSWindow alloc] initWithContentRect:NSMakeRect(0, 0, ActivityWidth, 160)
                                              styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable |
                                                        NSWindowStyleMaskResizable
                                                backing:NSBackingStoreBuffered
                                                  defer:YES];
    self.window.title = @"Runberth Activity";
    self.window.contentView = content;
    self.window.releasedWhenClosed = NO;
    self.window.delegate = self;
    [self.window center];
    __weak ActivityWindow *weakSelf = self;
    self.keyMonitor = [NSEvent addLocalMonitorForEventsMatchingMask:NSEventMaskKeyDown
                                                            handler:^NSEvent *(NSEvent *event) {
                                                              return [weakSelf textSizeKey:event] ? nil : event;
                                                            }];
}
/* ⌘+ or ⌘= larger, ⌘− smaller, ⌘0 back to 11 pt, from 10 to 18 pt. */
- (BOOL)textSizeKey:(NSEvent *)event {
    if (!self.window.keyWindow ||
        (event.modifierFlags & NSEventModifierFlagDeviceIndependentFlagsMask & ~NSEventModifierFlagShift) !=
            NSEventModifierFlagCommand)
        return NO;
    NSString *key = event.charactersIgnoringModifiers;
    CGFloat size = logFontSize();
    if ([key isEqualToString:@"+"] || [key isEqualToString:@"="])
        size = MIN(size + 1, 18);
    else if ([key isEqualToString:@"-"])
        size = MAX(size - 1, 10);
    else if ([key isEqualToString:@"0"])
        size = 11;
    else
        return NO;
    [NSUserDefaults.standardUserDefaults setDouble:size forKey:LogFontSizeKey];
    for (ActivityEntry *entry in self.entries.allValues)
        [entry restyle];
    return YES;
}
- (void)windowWillClose:(NSNotification *)notification {
    [self.clock invalidate];
    self.clock = nil;
}
- (void)update:(NSArray<NSDictionary *> *)activities {
    self.activities = activities;
    if (self.awaited) {
        for (NSDictionary *activity in activities) {
            if ([activity[@"running"] boolValue] && [activity[@"key"] isEqualToString:self.awaited]) {
                self.awaited = nil;
                [self showActivity:activity[@"id"]];
                return;
            }
        }
    }
    if (self.window.visible)
        [self redraw];
}
- (void)redraw {
    NSMutableSet *shown = [NSMutableSet set];
    NSMutableArray<NSView *> *order = [NSMutableArray array];
    // Newest first, a line between each.
    for (NSDictionary *activity in self.activities.reverseObjectEnumerator) {
        NSNumber *key = activity[@"id"];
        ActivityEntry *entry = self.entries[key];
        if (!entry) {
            entry = [[ActivityEntry alloc] init];
            __weak ActivityWindow *window = self;
            entry.resized = ^{
              [window fit];
            };
            self.entries[key] = entry;
        }
        [entry show:activity];
        [shown addObject:key];
        if (order.count)
            [order addObject:ruleLine()];
        [order addObject:entry.view];
    }
    for (NSNumber *key in self.entries.allKeys)
        if (![shown containsObject:key])
            [self.entries removeObjectForKey:key];
    if (!order.count)
        [order addObject:self.empty];
    // Rebuilt only when the entries change, not on every update.
    NSArray *views = [order filteredArrayUsingPredicate:[NSPredicate predicateWithBlock:^BOOL(id view, id __unused bindings) {
                              return ![view isKindOfClass:NSBox.class];
                            }]];
    NSArray *current = [self.list.arrangedSubviews
        filteredArrayUsingPredicate:[NSPredicate predicateWithBlock:^BOOL(id view, id __unused bindings) {
          return ![view isKindOfClass:NSBox.class];
        }]];
    if (![current isEqualToArray:views]) {
        for (NSView *view in self.list.arrangedSubviews.copy)
            [view removeFromSuperview];
        for (NSView *view in order) {
            [self.list addArrangedSubview:view];
            // Entries take the window's width, less its padding.
            if (view != self.empty)
                [view.widthAnchor constraintEqualToAnchor:self.list.widthAnchor constant:-2 * ActivityPadding]
                    .active = YES;
        }
    }
    [self fit];
}
/* The window takes the entries' size, keeping its top edge where it is,
 * so it never shows empty space or cuts an entry short. */
- (void)fit {
    if (!self.window)
        return;
    [self.list layoutSubtreeIfNeeded];
    CGFloat height = self.list.fittingSize.height;
    if (height == self.fittedHeight)
        return;
    self.fittedHeight = height;
    NSRect frame = self.window.frame;
    NSRect content = [self.window contentRectForFrameRect:frame];
    // The width stays as the user left it.
    NSRect fitted = [self.window frameRectForContentRect:NSMakeRect(0, 0, NSWidth(content), height)];
    fitted.origin.x = NSMinX(frame);
    fitted.origin.y = NSMaxY(frame) - NSHeight(fitted);
    [self.window setFrame:fitted display:YES animate:NO];
}
- (void)showActivity:(NSNumber *)activityId {
    if (!self.window)
        [self build];
    [self redraw];
    [NSApp activateIgnoringOtherApps:YES];
    [self.window makeKeyAndOrderFront:nil];
    [self.window orderFrontRegardless];
    if (!self.clock) {
        __weak ActivityWindow *window = self;
        self.clock = [NSTimer scheduledTimerWithTimeInterval:1
                                                     repeats:YES
                                                       block:^(NSTimer *__unused timer) {
                                                         for (ActivityEntry *entry in window.entries.allValues)
                                                             [entry tick];
                                                       }];
        // Keeps ticking while a menu is open or the window is dragged.
        [NSRunLoop.mainRunLoop addTimer:self.clock forMode:NSRunLoopCommonModes];
    }
}
@end

static ActivityWindow *activityWindow(void) {
    static ActivityWindow *shared;
    if (!shared)
        shared = [[ActivityWindow alloc] init];
    return shared;
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
@property NSTextField *tokenState, *loginNote, *version, *settingsPath;
/* Install Runberth…, enabled unless the server is known to have Runberth. */
@property NSButton *installButton;
/* Whether the server has Runberth: 1 yes, 0 no, -1 not known. */
@property int serverInstalled;
/* The install options in their sheet. */
@property NSPopUpButton *installSwap;
@property NSButton *installOnDemand;
/* Enabled only when there is a token to save, or a stored one to remove. */
@property NSButton *saveTokenButton, *removeTokenButton;
/* A typed token is on its way to the keychain: once it is stored, the field
 * shows dots again; if storing fails, it keeps what was typed. */
@property BOOL savingToken;
/* Whether macOS lets the app show notifications. */
@property NSTextField *notificationsStatus;
/* Whether `runberth` in /usr/local/bin is this app's command. */
@property NSTextField *commandLineStatus;
/* Installs the command, or removes the app's link once it is installed. */
@property NSButton *commandLineButton;

@property NSButton *cleanupButton;
/* Whether an install or a cleanup runs, which keeps its button off. */
@property BOOL installRunning, cleanupRunning;
/* In the cleanup dialog: the host typed to confirm, and the button it enables. */
@property NSTextField *confirmField;
@property NSButton *confirmButton;
@property NSAlert *confirmAlert;
/* A settings error, such as a host that is not one, under the Server
 * form until an edit fixes it. */
@property NSTextField *errorLine;
/* The activities Rust sent last, for the buttons they keep off. */
@property NSArray<NSDictionary *> *activities;
@property NSTimer *saveTimer;
@end

@implementation Settings
- (NSView *)server {
    self.host = textField(@"203.0.113.10 or vps.example.com", 260, self);
    self.user = textField(@"ubuntu", 140, self);
    self.port = textField(@"22", 70, self);
    self.key = textField(@"~/.ssh/id_ed25519", 260, self);
    NSTextField *keyNote = noteLabel();
    keyNote.stringValue = @"Leave empty to use your SSH agent and ~/.ssh/config. For localhost on port 22 "
                          @"the app runs Runberth on this computer, without SSH. The server's host key "
                          @"must already be known: connect once with ssh first.";
    self.installButton = pushButton(@"Install Runberth…", self, @selector(installRunberth:));
    self.errorLine = statusLine(360);
    NSGridView *grid = form(@[
        @[ @"Host:", self.host ],
        @[ @"User:", self.user ],
        @[ @"Port:", self.port ],
        @[ @"SSH key:", column(@[ row(@[ self.key, pushButton(@"Choose…", self, @selector(chooseKey:)) ]),
                                  keyNote ]) ],
        NSNull.null,
        @[ @"", row(@[ pushButton(@"Test Connection", tray, @selector(testConnection:)), self.installButton ]) ],
        @[ @"", self.errorLine ],
    ]);
    NSBox *line = [[NSBox alloc] init];
    line.boxType = NSBoxSeparator;
    NSView *cleanup = [self cleanupSection];
    NSStackView *stack = column(@[ grid, line, cleanup ]);
    stack.spacing = 18;
    // The line and the cleanup section span the form, so the red button's
    // right edge lines up with the Choose… button's.
    [NSLayoutConstraint activateConstraints:@[
        [line.widthAnchor constraintEqualToAnchor:grid.widthAnchor],
        [cleanup.widthAnchor constraintEqualToAnchor:grid.widthAnchor],
    ]];
    return stack;
}
/* Cleanup, which cannot be undone: a warning and what it removes, beside a
 * red button. The dialog it opens shows the server's full plan. */
- (NSView *)cleanupSection {
    NSImageView *icon = [NSImageView
        imageViewWithImage:[NSImage imageWithSystemSymbolName:@"exclamationmark.circle.fill"
                                     accessibilityDescription:@"Warning"]];
    icon.contentTintColor = appRed();
    NSTextField *warning = [NSTextField labelWithString:@"Warning: This action is destructive."];
    NSStackView *heading = row(@[ icon, warning ]);
    heading.spacing = 6;
    NSMutableArray<NSView *> *lines = [NSMutableArray arrayWithObject:heading];
    for (NSString *removed in @[
             @"Removes every runner and its GitHub registration.",
             @"Deletes runner accounts and caches.",
             @"Removes the packages and swap Runberth added.",
         ]) {
        NSTextField *bullet = [NSTextField labelWithString:[@"•  " stringByAppendingString:removed]];
        bullet.font = [NSFont systemFontOfSize:NSFont.smallSystemFontSize];
        bullet.textColor = NSColor.secondaryLabelColor;
        [lines addObject:bullet];
    }
    NSStackView *text = column(lines);
    text.spacing = 4;
    // The bullets line up with the warning's text, past its icon.
    for (NSView *view in lines)
        if (view != heading)
            [text setCustomSpacing:4 afterView:view];
    for (NSView *view in [lines subarrayWithRange:NSMakeRange(1, lines.count - 1)])
        [view.leadingAnchor constraintEqualToAnchor:warning.leadingAnchor].active = YES;

    // Red with white text, so it cannot pass for an ordinary button.
    self.cleanupButton = pushButton(@"Clean Up…", self, @selector(cleanup:));
    self.cleanupButton.hasDestructiveAction = YES;
    self.cleanupButton.bezelColor = appRed();
    self.cleanupButton.attributedTitle = [[NSAttributedString alloc]
        initWithString:self.cleanupButton.title
            attributes:@{NSForegroundColorAttributeName : NSColor.whiteColor,
                         NSFontAttributeName : self.cleanupButton.font}];
    [self.cleanupButton setContentHuggingPriority:NSLayoutPriorityRequired
                                   forOrientation:NSLayoutConstraintOrientationHorizontal];
    NSView *spacer = [[NSView alloc] init];
    [spacer setContentHuggingPriority:NSLayoutPriorityDefaultLow
                       forOrientation:NSLayoutConstraintOrientationHorizontal];
    NSStackView *section = [NSStackView stackViewWithViews:@[ text, spacer, self.cleanupButton ]];
    section.alignment = NSLayoutAttributeTop;
    return section;
}
/* Stands in for a stored token in the token field, which shows it as dots;
 * a GitHub token never contains "•", so typing or pasting replaces it. */
static NSString *const StoredTokenMask = @"••••••••••••••••••••";

- (NSView *)github {
    self.token = [[NSSecureTextField alloc] init];
    self.token.placeholderString = @"github_pat_… or ghp_…";
    self.token.delegate = self;
    [self.token.widthAnchor constraintEqualToConstant:260].active = YES;
    self.tokenState = statusLine(300);
    self.saveTokenButton = pushButton(@"Save", self, @selector(saveToken:));
    self.saveTokenButton.enabled = NO;
    self.removeTokenButton = pushButton(@"Remove Token", self, @selector(removeToken:));
    self.removeTokenButton.enabled = NO;
    NSTextField *tokenNote = noteLabel();
    tokenNote.stringValue = @"Showing runs needs Actions: Read. Connecting and disconnecting repositories "
                            @"also needs Administration: Read and write, and Contents: Read. A classic "
                            @"token with the repo scope covers both. It is kept in your login keychain.";
    self.tokenCommand = textField(@"gh auth token --user NAME", 260, self);
    NSTextField *commandNote = noteLabel();
    commandNote.stringValue = @"Optional. When set, the app runs this command for the token instead of "
                              @"reading the keychain.";
    return form(@[
        @[ @"Token:", column(@[ row(@[ self.token, self.saveTokenButton ]), self.tokenState, tokenNote ]) ],
        @[ @"", row(@[ pushButton(@"Create a Token…", self, @selector(createToken:)), self.removeTokenButton ]) ],
        NSNull.null,
        @[ @"Token command:", column(@[ self.tokenCommand, commandNote ]) ],
    ]);
}
- (NSView *)general {
    self.interval = textField(@"20", 60, self);
    self.notifications = [[NSPopUpButton alloc] init];
    for (NSArray *choice in @[ @[ @"Everything", @"all" ], @[ @"Failures and fixes", @"failures" ],
                               @[ @"Off", @"off" ] ]) {
        [self.notifications addItemWithTitle:choice[0]];
        self.notifications.lastItem.representedObject = choice[1];
    }
    self.notifications.target = self;
    self.notifications.action = @selector(changed:);
    self.loginBox = [NSButton checkboxWithTitle:@"Launch at Login" target:self action:@selector(toggleLogin:)];
    self.loginNote = statusLine(300);
    // Selectable, so the path can be copied.
    self.settingsPath = noteLabel();
    self.settingsPath.selectable = YES;
    return form(@[
        @[ @"Refresh every:", row(@[ self.interval, [NSTextField labelWithString:@"seconds"] ]) ],
        @[ @"Notifications:", [self notificationPermission] ],
        @[ @"Startup:", column(@[ self.loginBox, self.loginNote ]) ],
        @[ @"Command line:", [self commandLineTool] ],
        NSNull.null,
        @[ @"Settings file:", column(@[ pushButton(@"Open settings.json", self, @selector(openFile:)), self.settingsPath ]) ],
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
- (NSView *)support {
    // Ko-fi's logo from the app's asset catalog; a build without one shows a cup.
    NSImage *logo = [NSImage imageNamed:@"KoFi"];
    NSImageView *cup;
    if (logo) {
        cup = [NSImageView imageViewWithImage:logo];
        [cup.widthAnchor constraintEqualToAnchor:cup.heightAnchor multiplier:logo.size.width / logo.size.height]
            .active = YES;
    } else {
        NSImageSymbolConfiguration *large = [NSImageSymbolConfiguration configurationWithPointSize:34
                                                                                            weight:NSFontWeightRegular];
        cup = [NSImageView imageViewWithImage:[symbol(@"cup.and.saucer.fill", @"Ko-fi")
                                                  imageWithSymbolConfiguration:large]];
        cup.contentTintColor = NSColor.controlAccentColor;
    }
    cup.imageScaling = NSImageScaleProportionallyUpOrDown;
    // Both images get the same height, so the buttons below line up.
    [cup.heightAnchor constraintEqualToConstant:40].active = YES;
    // GitHub's mark from Primer Octicons (MIT), a template drawn in the text
    // colour; a build without it shows a star.
    NSImage *mark = [NSImage imageNamed:@"GitHub"] ?: symbol(@"star", nil);
    mark.accessibilityDescription = @"GitHub";
    NSImageView *github = [NSImageView imageViewWithImage:mark];
    github.contentTintColor = NSColor.labelColor;
    github.imageScaling = NSImageScaleProportionallyUpOrDown;
    [github.heightAnchor constraintEqualToConstant:40].active = YES;
    [github.widthAnchor constraintEqualToAnchor:github.heightAnchor].active = YES;
    NSButton *starButton = pushButton(@"Star on GitHub", self, @selector(openProject:));
    NSButton *coffeeButton = pushButton(@"Buy a Coffee", self, @selector(openDonate:));
    [starButton.widthAnchor constraintEqualToConstant:150].active = YES;
    [coffeeButton.widthAnchor constraintEqualToConstant:150].active = YES;
    NSStackView *(^option)(NSArray<NSView *> *) = ^NSStackView *(NSArray<NSView *> *views) {
      NSStackView *group = [NSStackView stackViewWithViews:views];
      group.orientation = NSUserInterfaceLayoutOrientationVertical;
      group.alignment = NSLayoutAttributeCenterX;
      group.spacing = 10;
      return group;
    };
    NSStackView *choices =
        [NSStackView stackViewWithViews:@[ option(@[ github, starButton ]), option(@[ cup, coffeeButton ]) ]];
    choices.orientation = NSUserInterfaceLayoutOrientationHorizontal;
    choices.alignment = NSLayoutAttributeTop;
    choices.distribution = NSStackViewDistributionFillEqually;
    choices.spacing = 24;
    NSTextField *free = [NSTextField wrappingLabelWithString:
        @"Runberth is free and open source. Every feature is available to everyone."];
    NSTextField *ask = [NSTextField wrappingLabelWithString:
        @"If you find it useful, star the project on GitHub or support its development with a coffee on Ko-fi."];
    free.preferredMaxLayoutWidth = ask.preferredMaxLayoutWidth = 400;
    // A problem goes to GitHub's bug report form, prefilled by the app.
    NSBox *line = [[NSBox alloc] init];
    line.boxType = NSBoxSeparator;
    NSTextField *problem = [NSTextField labelWithString:@"Something not working?"];
    NSStackView *report = row(@[ problem, pushButton(@"Report a Problem…", self, @selector(reportProblem:)) ]);
    report.alignment = NSLayoutAttributeCenterY;
    NSStackView *stack = column(@[ free, ask, choices, line, report ]);
    stack.spacing = 16;
    [stack setCustomSpacing:26 afterView:ask];
    [stack setCustomSpacing:24 afterView:choices];
    [choices.widthAnchor constraintEqualToConstant:380].active = YES;
    [line.widthAnchor constraintEqualToConstant:400].active = YES;
    return stack;
}
- (void)reportProblem:(id)sender {
    emit(@"report", @"");
}
- (void)openProject:(id)sender {
    [NSWorkspace.sharedWorkspace openURL:[NSURL URLWithString:ProjectURL]];
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
/* The permission macOS gives the app, beside a button to its page in
 * System Settings; both always take their space, so the window keeps its
 * size whatever the permission. */
/* The choice and the button to macOS's settings on one line, and whether
 * macOS allows notifications under them. */
- (NSView *)notificationPermission {
    self.notificationsStatus = statusLine(300);
    NSStackView *line = row(@[ self.notifications,
                               pushButton(@"Notification Settings…", self, @selector(openNotificationSettings:)) ]);
    line.alignment = NSLayoutAttributeCenterY;
    return column(@[ line, self.notificationsStatus ]);
}
- (void)refreshNotificationPermission {
    if (!tray.canNotify) {
        setStatus(self.notificationsStatus, @"Available in the Runberth app", YES);
        return;
    }
    [UNUserNotificationCenter.currentNotificationCenter
        getNotificationSettingsWithCompletionHandler:^(UNNotificationSettings *settings) {
          dispatch_async(dispatch_get_main_queue(), ^{
            switch (settings.authorizationStatus) {
            case UNAuthorizationStatusDenied:
                setStatus(self.notificationsStatus,
                          @"Off for Runberth in System Settings — notifications are not shown", NO);
                break;
            case UNAuthorizationStatusNotDetermined:
                setStatus(self.notificationsStatus, @"Not yet allowed", YES);
                break;
            default:
                setStatus(self.notificationsStatus, @"Allowed in System Settings", YES);
            }
          });
        }];
}
/* The `runberth` command: this app's binary, linked into /usr/local/bin. */
static NSString *const CommandLinePath = @"/usr/local/bin/runberth";

- (NSView *)commandLineTool {
    self.commandLineButton = pushButton(@"Install Command Line Tool…", self, @selector(toggleCommandLineTool:));
    self.commandLineStatus = statusLine(300);
    return column(@[ self.commandLineButton, self.commandLineStatus ]);
}
/* Whether there is a runberth command at /usr/local/bin, whatever put it
 * there: this app, another copy of it, or install.sh. */
- (BOOL)commandLineToolInstalled {
    // The link itself, so a link to an app that moved counts too.
    return [NSFileManager.defaultManager attributesOfItemAtPath:CommandLinePath error:NULL] != nil;
}
/* Where the installed command comes from. */
- (NSString *)commandLineSource {
    NSString *target = [NSFileManager.defaultManager destinationOfSymbolicLinkAtPath:CommandLinePath error:NULL];
    if (!target)
        return @"a separate copy";
    if ([target isEqualToString:NSBundle.mainBundle.executablePath])
        return @"linked to this app";
    NSString *inApp = @"/Contents/MacOS/runberth";
    NSString *app = [target hasSuffix:inApp] ? [target substringToIndex:target.length - inApp.length] : target;
    return [@"linked to " stringByAppendingString:app.stringByAbbreviatingWithTildeInPath];
}
- (void)refreshCommandLineTool {
    BOOL installed = [self commandLineToolInstalled];
    self.commandLineButton.title = installed ? @"Remove Command Line Tool…" : @"Install Command Line Tool…";
    if (installed)
    {
        NSString *source = [self commandLineSource];
        NSString *joiner = [source hasPrefix:@"linked"] ? @"Installed, " : @"Installed as ";
        setStatus(self.commandLineStatus, [joiner stringByAppendingString:source], YES);
    }
    else
        setStatus(self.commandLineStatus, @"Adds /usr/local/bin/runberth, linked to this app", YES);
}
/* While an install or a cleanup runs, its button stays off. */
- (void)showActivities:(NSArray<NSDictionary *> *)activities {
    self.activities = activities;
    BOOL installing = NO, cleaning = NO;
    for (NSDictionary *activity in activities) {
        if (![activity[@"running"] boolValue])
            continue;
        installing |= [activity[@"key"] isEqualToString:@"install"];
        cleaning |= [activity[@"key"] isEqualToString:@"cleanup"];
    }
    // Back on when it ends, unless the server does not need it.
    if (self.installRunning && !installing)
        self.installButton.enabled = self.serverInstalled != 1;
    if (self.cleanupRunning && !cleaning)
        self.cleanupButton.enabled = self.serverInstalled != 0;
    self.installRunning = installing;
    self.cleanupRunning = cleaning;
    if (installing)
        self.installButton.enabled = NO;
    if (cleaning)
        self.cleanupButton.enabled = NO;
}
/* What the command line button did, for the menu and the Activity window, as
 * every other activity. */
static void recordCommandLine(NSString *title, BOOL ok, NSString *result) {
    NSDictionary *done = @{@"key" : @"command-line", @"title" : title, @"ok" : @(ok), @"result" : result};
    NSData *json = [NSJSONSerialization dataWithJSONObject:done options:0 error:NULL];
    emit(@"activity-record", [[NSString alloc] initWithData:json encoding:NSUTF8StringEncoding]);
}
/* Runs a shell command as an administrator, after macOS asks for the password;
 * NO if it failed or was cancelled, with the failure shown. */
- (BOOL)runAsAdministrator:(NSString *)command {
    NSString *escaped = [[command stringByReplacingOccurrencesOfString:@"\\" withString:@"\\\\"]
        stringByReplacingOccurrencesOfString:@"\"" withString:@"\\\""];
    NSString *source = [NSString stringWithFormat:@"do shell script \"%@\" with administrator privileges", escaped];
    NSDictionary *failure = nil;
    [[[NSAppleScript alloc] initWithSource:source] executeAndReturnError:&failure];
    // Cancelling the password prompt is not an error worth showing.
    if (failure && [failure[NSAppleScriptErrorNumber] integerValue] != -128)
        recordCommandLine(@"Changing the runberth command", NO,
                          [@"Couldn't change the runberth command: "
                              stringByAppendingString:failure[NSAppleScriptErrorMessage] ?: @"the command failed"]);
    return failure == nil;
}
/* A path in single quotes for the shell. */
static NSString *shellQuoted(NSString *path) {
    return [NSString stringWithFormat:@"'%@'", [path stringByReplacingOccurrencesOfString:@"'" withString:@"'\\''"]];
}
- (void)toggleCommandLineTool:(id)sender {
    if ([self commandLineToolInstalled])
        [self removeCommandLineTool];
    else
        [self installCommandLineTool];
}
- (void)installCommandLineTool {
    NSString *executable = NSBundle.mainBundle.executablePath;
    NSFileManager *files = NSFileManager.defaultManager;
    NSDictionary *existing = [files attributesOfItemAtPath:CommandLinePath error:NULL];
    BOOL replaceable = !existing || [existing[NSFileType] isEqualToString:NSFileTypeSymbolicLink];
    if ([files isWritableFileAtPath:@"/usr/local/bin"] && replaceable) {
        NSError *error = nil;
        [files removeItemAtPath:CommandLinePath error:NULL];
        if (![files createSymbolicLinkAtPath:CommandLinePath withDestinationPath:executable error:&error]) {
            recordCommandLine(@"Installing the runberth command", NO,
                              [@"Couldn't install the runberth command: "
                                  stringByAppendingString:error.localizedDescription]);
            return;
        }
    } else if (![self runAsAdministrator:[NSString stringWithFormat:@"mkdir -p /usr/local/bin && ln -sf %@ %@",
                                                                     shellQuoted(executable),
                                                                     shellQuoted(CommandLinePath)]]) {
        return;
    }
    recordCommandLine(@"Installing the runberth command", YES,
                      @"Installed the runberth command: run runberth in Terminal");
    [self refreshCommandLineTool];
}
/* Removes the runberth command at /usr/local/bin, whatever installed it. */
- (void)removeCommandLineTool {
    NSAlert *alert = [[NSAlert alloc] init];
    alert.messageText = @"Remove the runberth command?";
    alert.informativeText = [NSString
        stringWithFormat:@"This removes /usr/local/bin/runberth, %@. The app keeps working, and you can install "
                         @"the command again here at any time.",
                         [self commandLineSource]];
    [alert addButtonWithTitle:@"Remove"];
    [alert addButtonWithTitle:@"Cancel"];
    [alert beginSheetModalForWindow:self.window
                  completionHandler:^(NSModalResponse response) {
                    if (response != NSAlertFirstButtonReturn || ![self commandLineToolInstalled])
                        return;
                    NSError *error = nil;
                    if ([NSFileManager.defaultManager isWritableFileAtPath:@"/usr/local/bin"]) {
                        if (![NSFileManager.defaultManager removeItemAtPath:CommandLinePath error:&error]) {
                            recordCommandLine(@"Removing the runberth command", NO,
                                              [@"Couldn't remove the runberth command: "
                                                  stringByAppendingString:error.localizedDescription]);
                            return;
                        }
                    } else if (![self runAsAdministrator:[@"rm -f " stringByAppendingString:shellQuoted(CommandLinePath)]]) {
                        return;
                    }
                    recordCommandLine(@"Removing the runberth command", YES, @"Removed the runberth command");
                    [self refreshCommandLineTool];
                  }];
}
- (void)openNotificationSettings:(id)sender {
    NSString *page = [@"x-apple.systempreferences:com.apple.Notifications-Settings.extension?id="
        stringByAppendingString:NSBundle.mainBundle.bundleIdentifier ?: @""];
    if (![NSWorkspace.sharedWorkspace openURL:[NSURL URLWithString:page]])
        [NSWorkspace.sharedWorkspace
            openURL:[NSURL URLWithString:@"x-apple.systempreferences:com.apple.preference.notifications"]];
}
- (void)windowDidBecomeKey:(NSNotification *)notification {
    // The keychain or GitHub may have changed while the window was away.
    emit(@"token-check", @"");
    [self refreshNotificationPermission];
    [self refreshCommandLineTool];
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
    [tabs addTabViewItem:pane(@"Support", symbol(@"heart", @"Support"), [self support])];
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
        setStatus(self.tokenState, @"Checking the token…", YES);
        self.version.stringValue = [NSString stringWithFormat:@"%@", state[@"version"]];
        // With the home folder as ~, as people write it.
        self.settingsPath.stringValue = [state[@"path"] ?: @"" stringByAbbreviatingWithTildeInPath];
    }
    [self refreshNotificationPermission];
    [self refreshCommandLineTool];
    [self refreshUpdates];
    [self status:state];
    emit(@"token-check", @"");
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
    NSScrollView *scroll = [[NSScrollView alloc] initWithFrame:NSMakeRect(0, 0, 460, 150)];
    scroll.hasVerticalScroller = YES;
    scroll.borderType = NSBezelBorder;
    NSTextView *text = [[NSTextView alloc] initWithFrame:scroll.contentView.bounds];
    text.editable = NO;
    text.font = [NSFont monospacedSystemFontOfSize:NSFont.smallSystemFontSize weight:NSFontWeightRegular];
    text.string = plan;
    text.autoresizingMask = NSViewWidthSizable;
    scroll.documentView = text;
    // As GitHub asks for a repository's name, the server's address confirms.
    NSString *host = [self.host.stringValue stringByTrimmingCharactersInSet:NSCharacterSet.whitespaceCharacterSet];
    NSTextField *ask = [NSTextField wrappingLabelWithString:
        [NSString stringWithFormat:@"To confirm, type %@ below:", host]];
    ask.preferredMaxLayoutWidth = 460;
    self.confirmField = [NSTextField textFieldWithString:@""];
    self.confirmField.placeholderString = host;
    self.confirmField.delegate = self;
    [self.confirmField.widthAnchor constraintEqualToConstant:460].active = YES;
    // Remove Everything is an ordinary button in the dialog's content: an
    // alert draws its own buttons in fixed colours, and this one turns red
    // once the host is typed.
    self.confirmButton = pushButton(@"Remove Everything", self, @selector(confirmRemoval:));
    // The size of the alert's own Cancel below it.
    self.confirmButton.controlSize = NSControlSizeLarge;
    [self.confirmButton.widthAnchor constraintEqualToConstant:228].active = YES;
    [self armConfirmButton:NO];
    NSStackView *accessory = column(@[ scroll, ask, self.confirmField, self.confirmButton ]);
    accessory.spacing = 8;
    [accessory setCustomSpacing:16 afterView:self.confirmField];
    [self.confirmButton.centerXAnchor constraintEqualToAnchor:accessory.centerXAnchor].active = YES;
    [scroll.widthAnchor constraintEqualToConstant:460].active = YES;
    [scroll.heightAnchor constraintEqualToConstant:150].active = YES;
    accessory.frame = NSMakeRect(0, 0, 460, accessory.fittingSize.height);
    alert.accessoryView = accessory;
    self.confirmField.toolTip = host;
    // The alert's own button: Escape cancels, and Return confirms nothing.
    NSButton *cancel = [alert addButtonWithTitle:@"Cancel"];
    cancel.keyEquivalent = @"\033";
    self.confirmAlert = alert;
    [alert layout];
    [alert.window makeFirstResponder:self.confirmField];
    [alert beginSheetModalForWindow:self.window
                  completionHandler:^(NSModalResponse response) {
                    self.confirmField = nil;
                    self.confirmButton = nil;
                    self.confirmAlert = nil;
                    if (response == NSModalResponseOK) {
                        self.cleanupButton.enabled = NO;
                        activityWindow().awaited = @"cleanup";
                        emit(@"cleanup-execute", @"");
                    } else {
                        self.cleanupButton.enabled = YES;
                    }
                  }];
}
/* The options for `runberth install`, which the app then runs, showing its
 * steps under Test Connection; when sudo on the server asks for a password,
 * it continues in Terminal. */
- (void)installRunberth:(id)sender {
    if (self.window.attachedSheet)
        return;
    // Install the server the window shows, not an edit still being typed.
    [self save];
    NSAlert *alert = [[NSAlert alloc] init];
    alert.messageText = @"Install Runberth on the server";
    alert.informativeText =
        @"This installs Runberth for this app's version, the sudoers rule the app needs, and a host config "
        @"for your GitHub token's account with the runner label runberth-ci. If sudo on the server asks for a "
        @"password, the install continues in Terminal.";
    self.installSwap = [[NSPopUpButton alloc] init];
    for (NSArray *choice in @[ @[ @"No swap", @0 ], @[ @"2 GB", @2 ], @[ @"3 GB", @3 ], @[ @"4 GB", @4 ] ]) {
        [self.installSwap addItemWithTitle:choice[0]];
        self.installSwap.lastItem.tag = [choice[1] integerValue];
    }
    [self.installSwap selectItemWithTag:3];
    self.installOnDemand = [NSButton checkboxWithTitle:@"Start runners only when a job arrives (experimental)"
                                                target:nil
                                                action:nil];
    self.installOnDemand.state = NSControlStateValueOn;
    NSTextField *swapNote = noteLabel();
    swapNote.stringValue = @"Room for builds on a small server.";
    NSTextField *onDemandNote = noteLabel();
    onDemandNote.stringValue = @"Saves about 70 MB per repository; a job waits a few seconds for its runner.";
    // Build packages come from each repository's .github/runberth.json when
    // it connects; the install itself adds only what every runner needs.
    NSTextField *packagesNote = noteLabel();
    packagesNote.stringValue = @"Installs git, curl, unzip, and ca-certificates. Each repository lists the "
                               @"apt packages its builds need in .github/runberth.json.";
    NSGridView *options = form(@[
        @[ @"Swap:", column(@[ self.installSwap, swapNote ]) ],
        @[ @"Runners:", column(@[ self.installOnDemand, onDemandNote ]) ],
        @[ @"Packages:", packagesNote ],
    ]);
    options.frame = NSMakeRect(0, 0, options.fittingSize.width, options.fittingSize.height);
    alert.accessoryView = options;
    [alert addButtonWithTitle:@"Install"];
    NSButton *cancel = [alert addButtonWithTitle:@"Cancel"];
    cancel.keyEquivalent = @"\033";
    [alert beginSheetModalForWindow:self.window
                  completionHandler:^(NSModalResponse response) {
                    if (response != NSAlertFirstButtonReturn)
                        return;
                    NSDictionary *choice = @{
                        @"swap" : @(self.installSwap.selectedTag),
                        @"on_demand" : @(self.installOnDemand.state == NSControlStateValueOn),
                    };
                    NSData *json = [NSJSONSerialization dataWithJSONObject:choice options:0 error:NULL];
                    self.installButton.enabled = NO;
                    activityWindow().awaited = @"install";
                    emit(@"install-server", [[NSString alloc] initWithData:json encoding:NSUTF8StringEncoding]);
                  }];
}
/* Remove Everything is a plain, greyed-out button until the host is typed,
 * then red with white text, as the Clean Up… button that opened it. */
/* Remove Everything, enabled only once the typed host matches. */
- (void)confirmRemoval:(id)sender {
    [self.window endSheet:self.confirmAlert.window returnCode:NSModalResponseOK];
}
- (void)armConfirmButton:(BOOL)armed {
    NSButton *button = self.confirmButton;
    button.enabled = armed;
    button.hasDestructiveAction = armed;
    button.bezelColor = armed ? appRed() : nil;
    NSDictionary *attributes = armed ? @{NSForegroundColorAttributeName : NSColor.whiteColor,
                                         NSFontAttributeName : button.font}
                                     : @{NSFontAttributeName : button.font};
    button.attributedTitle = [[NSAttributedString alloc] initWithString:@"Remove Everything" attributes:attributes];
}
- (void)status:(NSDictionary *)state {
    if (state[@"error"]) {
        setStatus(self.errorLine, state[@"error"], NO);
    }
    if (state[@"cleanup_plan"])
        [self confirmCleanup:state[@"cleanup_plan"]];
    if (state[@"installed"]) {
        // 1 installed, 0 not, -1 not known: each button stays available
        // unless the server is known not to need it.
        int installed = [state[@"installed"] intValue];
        self.serverInstalled = installed;
        self.installButton.enabled = installed != 1;
        self.installButton.toolTip =
            installed == 1 ? @"Runberth is installed: Update Server… in the menu updates it" : nil;
        if (self.installRunning)
            self.installButton.enabled = NO;
        self.cleanupButton.enabled = installed != 0 && !self.cleanupRunning;
        self.cleanupButton.toolTip = installed == 0 ? @"Runberth is not installed on this server" : nil;
    }
    if (state[@"activities"])
        [self showActivities:state[@"activities"]];
    if (state[@"token_stored"]) {
        if (self.savingToken && [state[@"token_stored"] boolValue] && [state[@"token_ok"] boolValue])
            self.token.stringValue = @"";
        self.savingToken = NO;
        [self showStoredToken:[state[@"token_stored"] boolValue]];
    }
    if (state[@"token_state"])
        setStatus(self.tokenState, state[@"token_state"], [state[@"token_ok"] boolValue]);
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
    if (notification.object == self.confirmField) {
        [self armConfirmButton:[self.confirmField.stringValue isEqualToString:self.confirmField.toolTip]];
        return;
    }
    if (notification.object == self.token) {
        // Typing over the stored token's dots starts a new token.
        NSString *typed = [self.token.stringValue stringByReplacingOccurrencesOfString:@"•" withString:@""];
        if (![typed isEqualToString:self.token.stringValue]) {
            NSText *editor = self.token.currentEditor;
            if (editor)
                editor.string = typed;
            else
                self.token.stringValue = typed;
        }
        self.saveTokenButton.enabled = typed.length > 0;
        return;
    }
    [self.saveTimer invalidate];
    self.saveTimer = [NSTimer scheduledTimerWithTimeInterval:0.6
                                                     repeats:NO
                                                       block:^(NSTimer *__unused timer) {
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
    NSString *token = self.token.stringValue;
    if (!token.length || [token containsString:@"•"])
        return;
    self.saveTokenButton.enabled = NO;
    self.savingToken = YES;
    setStatus(self.tokenState, @"Saving and checking the token…", YES);
    emit(@"token-save", token);
}
/* Dots in the token field while a token is stored, unless a new one is
 * being typed, and Remove only when there is something to remove. */
- (void)showStoredToken:(BOOL)stored {
    NSString *value = self.token.stringValue;
    BOOL typing = value.length && ![value isEqualToString:StoredTokenMask];
    if (!typing) {
        if (self.token.currentEditor)
            [self.window makeFirstResponder:nil];
        self.token.stringValue = stored ? StoredTokenMask : @"";
    }
    self.saveTokenButton.enabled = typing;
    self.removeTokenButton.enabled = stored;
}
/* Leaving the field empty keeps the stored token, so its dots come back. */
- (void)controlTextDidEndEditing:(NSNotification *)notification {
    if (notification.object == self.token && !self.token.stringValue.length && self.removeTokenButton.enabled)
        self.token.stringValue = StoredTokenMask;
}
- (void)removeToken:(id)sender {
    self.removeTokenButton.enabled = NO;
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
    // Its card shows the steps and the result.
    activityWindow().awaited = @"test";
    emit(@"settings-test", @"");
}
- (void)installSignalHandlers {
    signal(SIGTERM, SIG_IGN);
    signal(SIGINT, SIG_IGN);
    self.terminateSignal =
        dispatch_source_create(DISPATCH_SOURCE_TYPE_SIGNAL, SIGTERM, 0, dispatch_get_main_queue());
    self.interruptSignal =
        dispatch_source_create(DISPATCH_SOURCE_TYPE_SIGNAL, SIGINT, 0, dispatch_get_main_queue());
    // The system asking, as at logout, is not held up by a question.
    dispatch_source_set_event_handler(self.terminateSignal, ^{
      emit(@"quit-now", @"");
    });
    dispatch_source_set_event_handler(self.interruptSignal, ^{
      emit(@"quit-now", @"");
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
      NSDictionary *state = parse(json);
      [settings status:state];
      // The Activity window follows the work with or without Settings.
      if (state[@"activities"])
          [activityWindow() update:state[@"activities"]];
    });
}

void native_confirm_quit(const char *running) {
    NSString *titles = @(running);
    dispatch_async(dispatch_get_main_queue(), ^{
      NSAlert *alert = [[NSAlert alloc] init];
      alert.alertStyle = NSAlertStyleWarning;
      alert.messageText = @"Runberth is still working";
      alert.informativeText = [titles stringByAppendingString:
                                          @"\n\nQuitting stops this work partway, which can leave the server "
                                          @"half changed. Keep Runberth running until it ends."];
      // Return keeps it running; quitting takes a deliberate choice.
      [alert addButtonWithTitle:@"Keep Running"];
      NSButton *quit = [alert addButtonWithTitle:@"Quit Anyway"];
      quit.hasDestructiveAction = YES;
      quit.keyEquivalent = @"";
      [NSApp activateIgnoringOtherApps:YES];
      if ([alert runModal] == NSAlertSecondButtonReturn)
          emit(@"quit-now", @"");
    });
}

void native_show_activity(unsigned long long id) {
    dispatch_async(dispatch_get_main_queue(), ^{
      [activityWindow() showActivity:@(id)];
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
