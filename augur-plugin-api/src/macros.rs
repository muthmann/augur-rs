#[macro_export]
macro_rules! export_plugin {
    ($plugin_ty:ty) => {
        const _: () = {
            #[allow(non_camel_case_types)]
            struct __AugurExportedPlugin {
                plugin: $plugin_ty,
                schema_json: ::std::cell::RefCell<::std::vec::Vec<u8>>,
                setting_json: ::std::cell::RefCell<::std::vec::Vec<u8>>,
                status_json: ::std::cell::RefCell<::std::vec::Vec<u8>>,
                host_views_json: ::std::cell::RefCell<::std::vec::Vec<u8>>,
                host_view_dataset_json: ::std::cell::RefCell<::std::vec::Vec<u8>>,
            }

            impl __AugurExportedPlugin {
                fn new() -> Self {
                    Self {
                        plugin: <$plugin_ty as ::std::default::Default>::default(),
                        schema_json: ::std::cell::RefCell::new(::std::vec::Vec::new()),
                        setting_json: ::std::cell::RefCell::new(::std::vec::Vec::new()),
                        status_json: ::std::cell::RefCell::new(::std::vec::Vec::new()),
                        host_views_json: ::std::cell::RefCell::new(::std::vec::Vec::new()),
                        host_view_dataset_json: ::std::cell::RefCell::new(::std::vec::Vec::new()),
                    }
                }
            }

            unsafe fn __instance_ref(
                instance: *const ::std::ffi::c_void,
            ) -> Option<&'static __AugurExportedPlugin> {
                if instance.is_null() {
                    None
                } else {
                    Some(&*(instance.cast::<__AugurExportedPlugin>()))
                }
            }

            unsafe fn __instance_mut(
                instance: *mut ::std::ffi::c_void,
            ) -> Option<&'static mut __AugurExportedPlugin> {
                if instance.is_null() {
                    None
                } else {
                    Some(&mut *(instance.cast::<__AugurExportedPlugin>()))
                }
            }

            unsafe extern "C" fn __create() -> *mut ::std::ffi::c_void {
                ::std::panic::catch_unwind(|| {
                    ::std::boxed::Box::into_raw(
                        ::std::boxed::Box::new(__AugurExportedPlugin::new()),
                    ) as *mut ::std::ffi::c_void
                })
                .unwrap_or(::std::ptr::null_mut())
            }

            unsafe extern "C" fn __destroy(instance: *mut ::std::ffi::c_void) {
                let _ = ::std::panic::catch_unwind(|| {
                    if !instance.is_null() {
                        drop(::std::boxed::Box::from_raw(
                            instance.cast::<__AugurExportedPlugin>(),
                        ));
                    }
                });
            }

            unsafe extern "C" fn __name(instance: *const ::std::ffi::c_void) -> $crate::FfiString {
                ::std::panic::catch_unwind(|| {
                    __instance_ref(instance)
                        .map(|plugin| $crate::FfiString::from(plugin.plugin.name()))
                        .unwrap_or_default()
                })
                .unwrap_or_default()
            }

            unsafe extern "C" fn __description(
                instance: *const ::std::ffi::c_void,
            ) -> $crate::FfiString {
                ::std::panic::catch_unwind(|| {
                    __instance_ref(instance)
                        .map(|plugin| $crate::FfiString::from(plugin.plugin.description()))
                        .unwrap_or_default()
                })
                .unwrap_or_default()
            }

            unsafe extern "C" fn __enabled(instance: *const ::std::ffi::c_void) -> bool {
                ::std::panic::catch_unwind(|| {
                    __instance_ref(instance)
                        .map(|plugin| plugin.plugin.enabled())
                        .unwrap_or(false)
                })
                .unwrap_or(false)
            }

            unsafe extern "C" fn __set_enabled(instance: *mut ::std::ffi::c_void, enabled: bool) {
                let _ = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
                    if let Some(plugin) = __instance_mut(instance) {
                        plugin.plugin.set_enabled(enabled);
                    }
                }));
            }

            unsafe extern "C" fn __reset(instance: *mut ::std::ffi::c_void) {
                let _ = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
                    if let Some(plugin) = __instance_mut(instance) {
                        plugin.plugin.reset();
                    }
                }));
            }

            unsafe extern "C" fn __input_kind(
                instance: *const ::std::ffi::c_void,
            ) -> $crate::PluginInput {
                ::std::panic::catch_unwind(|| {
                    __instance_ref(instance)
                        .map(|plugin| plugin.plugin.input_kind())
                        .unwrap_or($crate::PluginInput::FrameOnly)
                })
                .unwrap_or($crate::PluginInput::FrameOnly)
            }

            unsafe extern "C" fn __num_dependencies(instance: *const ::std::ffi::c_void) -> usize {
                ::std::panic::catch_unwind(|| {
                    __instance_ref(instance)
                        .map(|plugin| plugin.plugin.dependencies().len())
                        .unwrap_or(0)
                })
                .unwrap_or(0)
            }

            unsafe extern "C" fn __dependency(
                instance: *const ::std::ffi::c_void,
                index: usize,
            ) -> $crate::FfiString {
                ::std::panic::catch_unwind(|| {
                    __instance_ref(instance)
                        .and_then(|plugin| {
                            plugin
                                .plugin
                                .dependencies()
                                .get(index)
                                .copied()
                                .map($crate::FfiString::from)
                        })
                        .unwrap_or_default()
                })
                .unwrap_or_default()
            }

            unsafe extern "C" fn __process_frame(
                instance: *mut ::std::ffi::c_void,
                frame: *const $crate::FfiPreviewFrame,
                output: *mut $crate::FfiOutputCallbacks,
                context: *mut $crate::FfiPluginContext,
                event_store: *const $crate::FfiEventStoreHandle,
            ) {
                let _ = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
                    let Some(plugin) = __instance_mut(instance) else {
                        return;
                    };
                    let Some(frame) = frame.as_ref() else {
                        return;
                    };
                    let Some(output) = output.as_mut() else {
                        return;
                    };
                    let Some(context) = context.as_mut() else {
                        return;
                    };
                    let Some(event_store) = event_store.as_ref() else {
                        return;
                    };

                    let frame = $crate::PluginFrame::new(frame);
                    let mut output = $crate::HostOutput::new(output);
                    let mut context = $crate::HostContext::new(context);
                    let event_store = $crate::EventStoreHandle::new(event_store);
                    plugin
                        .plugin
                        .process_frame(&frame, &mut output, &mut context, &event_store);
                }));
            }

            unsafe extern "C" fn __settings_schema(
                instance: *const ::std::ffi::c_void,
                out_ptr: *mut *const u8,
                out_len: *mut usize,
            ) {
                let result = ::std::panic::catch_unwind(|| {
                    let Some(plugin) = __instance_ref(instance) else {
                        unsafe {
                            $crate::__private::clear_out_bytes(out_ptr, out_len);
                        }
                        return;
                    };
                    let json =
                        ::serde_json::to_vec(&plugin.plugin.settings_schema()).unwrap_or_default();
                    unsafe {
                        $crate::__private::write_bytes(&plugin.schema_json, json, out_ptr, out_len);
                    }
                });
                if result.is_err() {
                    unsafe {
                        $crate::__private::clear_out_bytes(out_ptr, out_len);
                    }
                }
            }

            unsafe extern "C" fn __get_setting(
                instance: *const ::std::ffi::c_void,
                key: $crate::FfiString,
                out_ptr: *mut *const u8,
                out_len: *mut usize,
            ) -> bool {
                ::std::panic::catch_unwind(|| {
                    let Some(plugin) = __instance_ref(instance) else {
                        return false;
                    };
                    let Ok(key) = key.as_str() else {
                        return false;
                    };
                    let Some(value) = plugin.plugin.get_setting(key) else {
                        return false;
                    };
                    let json = ::serde_json::to_vec(&value).unwrap_or_default();
                    unsafe {
                        $crate::__private::write_bytes(
                            &plugin.setting_json,
                            json,
                            out_ptr,
                            out_len,
                        );
                    }
                    true
                })
                .unwrap_or(false)
            }

            unsafe extern "C" fn __set_setting(
                instance: *mut ::std::ffi::c_void,
                key: $crate::FfiString,
                value: $crate::FfiSlice<u8>,
            ) -> bool {
                ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
                    let Some(plugin) = __instance_mut(instance) else {
                        return false;
                    };
                    let Ok(key) = key.as_str() else {
                        return false;
                    };
                    let Ok(value) =
                        ::serde_json::from_slice::<::serde_json::Value>(value.as_slice())
                    else {
                        return false;
                    };
                    plugin.plugin.set_setting(key, value).is_ok()
                }))
                .unwrap_or(false)
            }

            unsafe extern "C" fn __status_entries(
                instance: *const ::std::ffi::c_void,
                out_ptr: *mut *const u8,
                out_len: *mut usize,
            ) {
                let result = ::std::panic::catch_unwind(|| {
                    let Some(plugin) = __instance_ref(instance) else {
                        unsafe {
                            $crate::__private::clear_out_bytes(out_ptr, out_len);
                        }
                        return;
                    };
                    let json =
                        ::serde_json::to_vec(&plugin.plugin.status_entries()).unwrap_or_default();
                    unsafe {
                        $crate::__private::write_bytes(&plugin.status_json, json, out_ptr, out_len);
                    }
                });
                if result.is_err() {
                    unsafe {
                        $crate::__private::clear_out_bytes(out_ptr, out_len);
                    }
                }
            }

            unsafe extern "C" fn __host_views(
                instance: *const ::std::ffi::c_void,
                out_ptr: *mut *const u8,
                out_len: *mut usize,
            ) {
                let result = ::std::panic::catch_unwind(|| {
                    let Some(plugin) = __instance_ref(instance) else {
                        unsafe {
                            $crate::__private::clear_out_bytes(out_ptr, out_len);
                        }
                        return;
                    };
                    let json =
                        ::serde_json::to_vec(&plugin.plugin.host_views()).unwrap_or_default();
                    unsafe {
                        $crate::__private::write_bytes(
                            &plugin.host_views_json,
                            json,
                            out_ptr,
                            out_len,
                        );
                    }
                });
                if result.is_err() {
                    unsafe {
                        $crate::__private::clear_out_bytes(out_ptr, out_len);
                    }
                }
            }

            unsafe extern "C" fn __host_view_dataset(
                instance: *const ::std::ffi::c_void,
                dataset_id: $crate::FfiString,
                out_ptr: *mut *const u8,
                out_len: *mut usize,
            ) -> bool {
                ::std::panic::catch_unwind(|| {
                    let Some(plugin) = __instance_ref(instance) else {
                        unsafe {
                            $crate::__private::clear_out_bytes(out_ptr, out_len);
                        }
                        return false;
                    };
                    let Ok(dataset_id) = dataset_id.as_str() else {
                        unsafe {
                            $crate::__private::clear_out_bytes(out_ptr, out_len);
                        }
                        return false;
                    };
                    let Some(bytes) = plugin.plugin.host_view_dataset(dataset_id) else {
                        unsafe {
                            $crate::__private::clear_out_bytes(out_ptr, out_len);
                        }
                        return false;
                    };
                    unsafe {
                        $crate::__private::write_bytes(
                            &plugin.host_view_dataset_json,
                            bytes,
                            out_ptr,
                            out_len,
                        );
                    }
                    true
                })
                .unwrap_or_else(|_| {
                    unsafe {
                        $crate::__private::clear_out_bytes(out_ptr, out_len);
                    }
                    false
                })
            }

            static __AUGUR_PLUGIN_VTABLE: $crate::PluginVTable = $crate::PluginVTable {
                vtable_size: ::std::mem::size_of::<$crate::PluginVTable>(),
                create: __create,
                destroy: __destroy,
                name: __name,
                description: __description,
                enabled: __enabled,
                set_enabled: __set_enabled,
                reset: __reset,
                input_kind: __input_kind,
                num_dependencies: __num_dependencies,
                dependency: __dependency,
                process_frame: __process_frame,
                settings_schema: __settings_schema,
                get_setting: __get_setting,
                set_setting: __set_setting,
                status_entries: __status_entries,
                host_views: __host_views,
                host_view_dataset: __host_view_dataset,
            };

            #[no_mangle]
            pub extern "C" fn augur_plugin_vtable() -> *const $crate::PluginVTable {
                &__AUGUR_PLUGIN_VTABLE
            }
        };
    };
}
