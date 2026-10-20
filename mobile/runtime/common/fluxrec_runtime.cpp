#include <dlfcn.h>
#include <pthread.h>
#include <stdint.h>
#include <string.h>
#include <unistd.h>

#include <algorithm>
#include <string>
#include <vector>

#if defined(__ANDROID__)
#include <android/log.h>
#include "And64InlineHook.hpp"
#define LOGI(...) __android_log_print(ANDROID_LOG_INFO, "FLUXREC", __VA_ARGS__)
#define LOGE(...) __android_log_print(ANDROID_LOG_ERROR, "FLUXREC", __VA_ARGS__)
#define IL2CPP_LIBRARY "libil2cpp.so"
static void install_code_hook(void *target, void *replacement, void **original) {
    A64HookFunction(target, replacement, original);
}
#elif defined(__APPLE__)
#include <stdio.h>
#include "dobby.h"
#define LOGI(...) do { fprintf(stderr, "[FLUXREC] "); fprintf(stderr, __VA_ARGS__); fprintf(stderr, "\n"); } while (0)
#define LOGE(...) do { fprintf(stderr, "[FLUXREC ERROR] "); fprintf(stderr, __VA_ARGS__); fprintf(stderr, "\n"); } while (0)
#define IL2CPP_LIBRARY nullptr
static void install_code_hook(void *target, void *replacement, void **original) {
    DobbyHook(target, replacement, original);
}
#else
#error Flux Rec mobile runtime supports Android and iOS only.
#endif

namespace {

using DomainGet = void *(*)();
using DomainGetAssemblies = const void **(*)(const void *, size_t *);
using AssemblyGetImage = const void *(*)(const void *);
using ImageGetClassCount = size_t (*)(const void *);
using ImageGetClass = void *(*)(const void *, size_t);
using ClassGetName = const char *(*)(void *);
using ClassGetNamespace = const char *(*)(void *);
using ClassGetMethods = const void *(*)(void *, void **);
using ClassGetFields = void *(*)(void *, void **);
using ClassGetParent = void *(*)(void *);
using MethodGetName = const char *(*)(const void *);
using MethodGetParamCount = uint32_t (*)(const void *);
using MethodGetReturnType = const void *(*)(const void *);
using MethodGetFlags = uint32_t (*)(const void *, uint32_t *);
using TypeGetName = char *(*)(const void *);
using FieldGetName = const char *(*)(void *);
using FieldGetType = const void *(*)(void *);
using FieldGetValue = void (*)(void *, void *, void *);
using FieldSetValue = void (*)(void *, void *, void *);
using ObjectGetClass = void *(*)(void *);
using ObjectNew = void *(*)(void *);
using StringNew = void *(*)(const char *);
using RuntimeInvoke = void *(*)(const void *, void *, void **, void **);
using Il2CppFree = void (*)(void *);

struct Api {
    DomainGet domain_get{};
    DomainGetAssemblies domain_get_assemblies{};
    AssemblyGetImage assembly_get_image{};
    ImageGetClassCount image_get_class_count{};
    ImageGetClass image_get_class{};
    ClassGetName class_get_name{};
    ClassGetNamespace class_get_namespace{};
    ClassGetMethods class_get_methods{};
    ClassGetFields class_get_fields{};
    ClassGetParent class_get_parent{};
    MethodGetName method_get_name{};
    MethodGetParamCount method_get_param_count{};
    MethodGetReturnType method_get_return_type{};
    MethodGetFlags method_get_flags{};
    TypeGetName type_get_name{};
    FieldGetName field_get_name{};
    FieldGetType field_get_type{};
    FieldGetValue field_get_value{};
    FieldSetValue field_set_value{};
    ObjectGetClass object_get_class{};
    ObjectNew object_new{};
    StringNew string_new{};
    RuntimeInvoke runtime_invoke{};
    Il2CppFree free{};
} api;

void *il2cpp_handle;

template <typename T>
bool bind(T &slot, const char *name) {
    slot = reinterpret_cast<T>(dlsym(il2cpp_handle, name));
    if (!slot) LOGE("missing IL2CPP export %s", name);
    return slot != nullptr;
}

bool bind_api() {
    il2cpp_handle = dlopen(IL2CPP_LIBRARY, RTLD_NOW);
    if (!il2cpp_handle) return false;
    return bind(api.domain_get, "il2cpp_domain_get") &&
           bind(api.domain_get_assemblies, "il2cpp_domain_get_assemblies") &&
           bind(api.assembly_get_image, "il2cpp_assembly_get_image") &&
           bind(api.image_get_class_count, "il2cpp_image_get_class_count") &&
           bind(api.image_get_class, "il2cpp_image_get_class") &&
           bind(api.class_get_name, "il2cpp_class_get_name") &&
           bind(api.class_get_namespace, "il2cpp_class_get_namespace") &&
           bind(api.class_get_methods, "il2cpp_class_get_methods") &&
           bind(api.class_get_fields, "il2cpp_class_get_fields") &&
           bind(api.class_get_parent, "il2cpp_class_get_parent") &&
           bind(api.method_get_name, "il2cpp_method_get_name") &&
           bind(api.method_get_param_count, "il2cpp_method_get_param_count") &&
           bind(api.method_get_return_type, "il2cpp_method_get_return_type") &&
           bind(api.method_get_flags, "il2cpp_method_get_flags") &&
           bind(api.type_get_name, "il2cpp_type_get_name") &&
           bind(api.field_get_name, "il2cpp_field_get_name") &&
           bind(api.field_get_type, "il2cpp_field_get_type") &&
           bind(api.field_get_value, "il2cpp_field_get_value") &&
           bind(api.field_set_value, "il2cpp_field_set_value") &&
           bind(api.object_get_class, "il2cpp_object_get_class") &&
           bind(api.object_new, "il2cpp_object_new") &&
           bind(api.string_new, "il2cpp_string_new") &&
           bind(api.runtime_invoke, "il2cpp_runtime_invoke") &&
           bind(api.free, "il2cpp_free");
}

std::vector<void *> all_classes() {
    std::vector<void *> out;
    size_t assembly_count = 0;
    auto assemblies = api.domain_get_assemblies(api.domain_get(), &assembly_count);
    for (size_t a = 0; a < assembly_count; ++a) {
        const void *image = api.assembly_get_image(assemblies[a]);
        size_t count = api.image_get_class_count(image);
        for (size_t i = 0; i < count; ++i) out.push_back(api.image_get_class(image, i));
    }
    return out;
}

void *find_class(const std::vector<void *> &classes, const char *name, const char *namesp = nullptr) {
    for (void *klass : classes) {
        const char *candidate = api.class_get_name(klass);
        const char *candidate_ns = api.class_get_namespace(klass);
        if (candidate && strcmp(candidate, name) == 0 &&
            (!namesp || (candidate_ns && strcmp(candidate_ns, namesp) == 0))) return klass;
    }
    return nullptr;
}

const void *find_method(void *klass, const char *name, int parameter_count) {
    if (!klass) return nullptr;
    void *iter = nullptr;
    while (const void *method = api.class_get_methods(klass, &iter)) {
        const char *candidate = api.method_get_name(method);
        if (candidate && strcmp(candidate, name) == 0 &&
            static_cast<int>(api.method_get_param_count(method)) == parameter_count) return method;
    }
    return nullptr;
}

std::string type_name(const void *type) {
    char *raw = type ? api.type_get_name(type) : nullptr;
    if (!raw) return {};
    std::string result(raw);
    api.free(raw);
    return result;
}

void *find_field(void *klass, const char *exact, const char *type_fragment = nullptr) {
    for (void *current = klass; current; current = api.class_get_parent(current)) {
        void *iter = nullptr;
        while (void *field = api.class_get_fields(current, &iter)) {
            const char *name = api.field_get_name(field);
            if (exact && name && (strcmp(name, exact) == 0 || strstr(name, exact) != nullptr)) return field;
            if (type_fragment && type_name(api.field_get_type(field)).find(type_fragment) != std::string::npos) return field;
        }
    }
    return nullptr;
}

bool hook_method(const void *method, void *replacement, void **original, const char *label) {
    if (!method) {
        LOGE("method not found: %s", label);
        return false;
    }
    void *address = *reinterpret_cast<void *const *>(method);
    if (!address) {
        LOGE("method has no code pointer: %s", label);
        return false;
    }
    install_code_hook(address, replacement, original);
    LOGI("hooked %s at %p", label, address);
    return true;
}

std::string managed_string(void *value) {
    if (!value) return {};
    int32_t length = *reinterpret_cast<int32_t *>(reinterpret_cast<uint8_t *>(value) + 0x10);
    const uint16_t *chars = reinterpret_cast<const uint16_t *>(reinterpret_cast<uint8_t *>(value) + 0x14);
    std::string result;
    result.reserve(length > 0 ? static_cast<size_t>(length) : 0);
    for (int32_t i = 0; i < length; ++i) result.push_back(chars[i] < 0x80 ? static_cast<char>(chars[i]) : '?');
    return result;
}

using HttpSend = void *(*)(void *);
HttpSend original_http_send;
void *uri_field;
const void *uri_to_string;
const void *uri_ctor;
void *uri_class;

void *hook_http_send(void *request) {
    if (request && uri_field && uri_to_string && uri_ctor && uri_class) {
        void *uri = nullptr;
        api.field_get_value(request, uri_field, &uri);
        if (uri) {
            void *exception = nullptr;
            void *text = api.runtime_invoke(uri_to_string, uri, nullptr, &exception);
            std::string url = exception ? std::string() : managed_string(text);
            const std::string official = "ns.rec.net";
            size_t at = url.find(official);
            if (at != std::string::npos) {
                url.replace(at, official.size(), FLUXREC_SERVER_HOST);
                void *new_uri = api.object_new(uri_class);
                void *new_text = api.string_new(url.c_str());
                void *args[] = {new_text};
                exception = nullptr;
                api.runtime_invoke(uri_ctor, new_uri, args, &exception);
                if (!exception) {
                    api.field_set_value(request, uri_field, &new_uri);
                    LOGI("redirected name server to %s", FLUXREC_SERVER_HOST);
                } else {
                    LOGE("failed to construct redirected System.Uri");
                }
            }
        }
    }
    return original_http_send(request);
}

using PhotonFactory = void *(*)(void *);
PhotonFactory original_photon_factory;

void set_string_field(void *object, void *klass, const char *name, const char *value) {
    if (void *field = find_field(klass, name)) {
        void *string = api.string_new(value);
        api.field_set_value(object, field, &string);
    }
}

void *hook_photon_factory(void *self) {
    void *settings = original_photon_factory(self);
    if (!settings) return settings;
    void *klass = api.object_get_class(settings);
    set_string_field(settings, klass, "AppIdRealtime", FLUXREC_PHOTON_REALTIME);
    set_string_field(settings, klass, "AppIdVoice", FLUXREC_PHOTON_VOICE);
    set_string_field(settings, klass, "AppIdChat", FLUXREC_PHOTON_CHAT);
    set_string_field(settings, klass, "FixedRegion", FLUXREC_PHOTON_REGION);
    if (void *field = find_field(klass, "UseNameServer")) {
        bool enabled = true;
        api.field_set_value(settings, field, &enabled);
    }
    LOGI("Photon configured: region=%s realtime=%s voice=%s", FLUXREC_PHOTON_REGION,
         FLUXREC_PHOTON_REALTIME, FLUXREC_PHOTON_VOICE);
    return settings;
}

using NotifyCertificate = void (*)(void *, void *);
NotifyCertificate original_notify_certificate;
void hook_notify_certificate(void *, void *) {
    LOGI("accepted self-hosted RecNet certificate");
}

const void *find_photon_factory(const std::vector<void *> &classes) {
    for (void *klass : classes) {
        void *iter = nullptr;
        while (const void *method = api.class_get_methods(klass, &iter)) {
            if (api.method_get_param_count(method) != 0) continue;
            uint32_t implementation = 0;
            uint32_t flags = api.method_get_flags(method, &implementation);
            if ((flags & 0x10u) != 0) continue;
            std::string returned = type_name(api.method_get_return_type(method));
            if (returned.find("Photon.Realtime.AppSettings") != std::string::npos) return method;
        }
    }
    return nullptr;
}

bool install_hooks() {
    auto classes = all_classes();
    if (classes.empty()) return false;

    void *http_manager = find_class(classes, "HTTPManager", "BestHTTP");
    const void *send = find_method(http_manager, "SendRequest", 1);
    if (send) {
        void *request_class = find_class(classes, "HTTPRequest", "BestHTTP");
        uri_field = find_field(request_class, nullptr, "System.Uri");
        uri_class = find_class(classes, "Uri", "System");
        uri_to_string = find_method(uri_class, "ToString", 0);
        uri_ctor = find_method(uri_class, ".ctor", 1);
    }
    const void *photon = find_photon_factory(classes);
    void *tls = find_class(classes, "LegacyTlsAuthentication");
    const void *notify = find_method(tls, "NotifyServerCertificate", 1);

    // Do not partially arm a build and retry over already-hooked code. Unity loads
    // assemblies in stages, so wait until every required target is visible first.
    if (!send || !uri_field || !uri_class || !uri_to_string || !uri_ctor || !photon || !notify) return false;

    bool http_ok = hook_method(send, reinterpret_cast<void *>(hook_http_send),
                               reinterpret_cast<void **>(&original_http_send), "BestHTTP.HTTPManager.SendRequest");
    bool photon_ok = hook_method(photon, reinterpret_cast<void *>(hook_photon_factory),
                                 reinterpret_cast<void **>(&original_photon_factory), "Photon AppSettings factory");
    bool tls_ok = hook_method(notify, reinterpret_cast<void *>(hook_notify_certificate),
                             reinterpret_cast<void **>(&original_notify_certificate), "LegacyTlsAuthentication.NotifyServerCertificate");
    return http_ok && photon_ok && tls_ok;
}

void *bootstrap(void *) {
    for (int attempt = 0; attempt < 300; ++attempt) {
        if (bind_api() && api.domain_get() && install_hooks()) {
            LOGI("Flux Rec mobile hooks active");
            return nullptr;
        }
        if (il2cpp_handle) {
            dlclose(il2cpp_handle);
            il2cpp_handle = nullptr;
        }
        sleep(1);
    }
    LOGE("Flux Rec mobile hooks could not be installed");
    return nullptr;
}

}  // namespace

__attribute__((constructor)) static void fluxrec_mobile_start() {
    pthread_t thread;
    if (pthread_create(&thread, nullptr, bootstrap, nullptr) == 0) pthread_detach(thread);
}
