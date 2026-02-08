#[cfg(target_os = "macos")]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Sel};
    use objc2::{msg_send_id, sel, ClassType};
    use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem, NSEventModifierFlags};
    use objc2_foundation::{MainThreadMarker, NSString, NSObject};

    static OPEN_SETTINGS_REQUEST: AtomicBool = AtomicBool::new(false);
    static MENU_INSTALLED: OnceLock<()> = OnceLock::new();
    static MENU_TARGET: OnceLock<usize> = OnceLock::new();

    pub fn install_settings_menu() {
        MENU_INSTALLED.get_or_init(|| {
            install_settings_menu_inner();
        });
    }

    pub fn take_open_settings_request() -> bool {
        OPEN_SETTINGS_REQUEST.swap(false, Ordering::Relaxed)
    }

    fn install_settings_menu_inner() {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        let Some(main_menu) = (unsafe { app.mainMenu() }) else {
            return;
        };
        let Some(app_item) = (unsafe { main_menu.itemAtIndex(0) }) else {
            return;
        };
        let Some(app_menu) = (unsafe { app_item.submenu() }) else {
            return;
        };

        let target_ptr = MENU_TARGET.get_or_init(|| Retained::into_raw(create_menu_target()) as usize);
        let target = unsafe { &*(*target_ptr as *mut AnyObject) };

        if let Some(existing_item) = find_existing_preferences(&app_menu) {
            unsafe {
                existing_item.setTarget(Some(target));
                existing_item.setAction(Some(sel!(openSettings:)));
            }
            return;
        }
        let title = NSString::from_str("設定...");
        let key_equivalent = NSString::from_str(",");
        let item = mtm.alloc::<NSMenuItem>();
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
            item,
            &title,
            Some(sel!(openSettings:)),
            &key_equivalent,
            )
        };
        unsafe {
            item.setTarget(Some(target));
        }
        item.setKeyEquivalentModifierMask(NSEventModifierFlags::NSEventModifierFlagCommand);

        let count = unsafe { app_menu.numberOfItems() };
        let insert_index = if count > 1 { 1 } else { count };
        unsafe {
            app_menu.insertItem_atIndex(&item, insert_index);
        }
    }

    fn find_existing_preferences(menu: &NSMenu) -> Option<Retained<NSMenuItem>> {
        let titles = ["設定...", "Preferences...", "環境設定..."];
        for title in titles {
            let ns_title = NSString::from_str(title);
            let index = unsafe { menu.indexOfItemWithTitle(&ns_title) };
            if index >= 0 {
                return unsafe { menu.itemAtIndex(index) };
            }
        }
        None
    }

    fn create_menu_target() -> Retained<AnyObject> {
        let cls = menu_target_class();
        unsafe { msg_send_id![cls, new] }
    }

    fn menu_target_class() -> &'static AnyClass {
        static CLASS: OnceLock<&AnyClass> = OnceLock::new();
        CLASS.get_or_init(|| {
            let superclass = NSObject::class();
            let mut builder =
                ClassBuilder::new("VJDownloaderMenuTarget", superclass).expect("class");
            unsafe {
                builder.add_method(
                    sel!(openSettings:),
                    open_settings as extern "C" fn(_, _, _),
                );
            }
            builder.register()
        })
    }

    extern "C" fn open_settings(_this: &AnyObject, _sel: Sel, _sender: *mut AnyObject) {
        OPEN_SETTINGS_REQUEST.store(true, Ordering::Relaxed);
    }
}

#[cfg(target_os = "macos")]
pub use imp::{install_settings_menu, take_open_settings_request};

#[cfg(not(target_os = "macos"))]
pub fn install_settings_menu() {}

#[cfg(not(target_os = "macos"))]
pub fn take_open_settings_request() -> bool {
    false
}
