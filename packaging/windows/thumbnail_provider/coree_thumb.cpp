// SPDX-License-Identifier: MIT OR Apache-2.0
// Clean-room Win32 COM in-process IThumbnailProvider + IInitializeWithStream server
// for coree (.coree, .cdr, .cdt, .cmx, .cpt). Delegates preview decoding/rendering
// to sibling coree.com (fallback coree.exe) and converts PNG output to 32-bpp
// premultiplied ARGB HBITMAP via Windows Imaging Component (WIC).

#ifndef UNICODE
#define UNICODE
#endif
#ifndef _UNICODE
#define _UNICODE
#endif
#define WIN32_LEAN_AND_MEAN

#include <windows.h>
#include <shlwapi.h>
#include <shlobj.h>
#include <thumbcache.h>
#include <propsys.h>
#include <wincodec.h>
#include <objbase.h>

#include <atomic>
#include <cstdio>
#include <cstring>
#include <new>
#include <string>
#include <vector>

namespace {

constexpr size_t kMaxStreamBytes = 64u * 1024u * 1024u; // 64 MiB hard bound
constexpr DWORD kCliTimeoutMs = 8000;

// {B4E8A7C2-5F19-4D3A-9C6E-1A2B3C4D5E6F}
const CLSID CLSID_CoreeThumbProvider = {
    0xB4E8A7C2, 0x5F19, 0x4D3A, {0x9C, 0x6E, 0x1A, 0x2B, 0x3C, 0x4D, 0x5E, 0x6F}
};

constexpr wchar_t kClsidStr[] = L"{B4E8A7C2-5F19-4D3A-9C6E-1A2B3C4D5E6F}";
constexpr wchar_t kProviderDesc[] = L"coree Thumbnail Provider";
constexpr wchar_t kThumbProviderIidStr[] = L"{e357fccd-a995-4576-b01f-234630154e96}";
constexpr const wchar_t* kThumbExtensions[] = {
    L".coree", L".cdr", L".cdt", L".cmx", L".cpt", L"coree.Document"
};

HMODULE g_hModule = nullptr;
std::atomic<LONG> g_cDllRef{0};

std::wstring SiblingExePath(const wchar_t* exeName) {
    wchar_t modPath[MAX_PATH] = {};
    DWORD len = GetModuleFileNameW(g_hModule, modPath, MAX_PATH);
    if (len == 0 || len >= MAX_PATH) {
        return {};
    }
    PathRemoveFileSpecW(modPath);
    if (!PathAppendW(modPath, exeName)) {
        return {};
    }
    if (GetFileAttributesW(modPath) != INVALID_FILE_ATTRIBUTES) {
        return modPath;
    }
    return {};
}

bool MakeTempFilePath(const wchar_t* prefix, const wchar_t* ext, std::wstring* out) {
    wchar_t tempDir[MAX_PATH] = {};
    DWORD dirLen = GetTempPathW(MAX_PATH, tempDir);
    if (dirLen == 0 || dirLen >= MAX_PATH) {
        return false;
    }
    wchar_t tmpFile[MAX_PATH] = {};
    if (GetTempFileNameW(tempDir, prefix, 0, tmpFile) == 0) {
        return false;
    }
    DeleteFileW(tmpFile);
    std::wstring p(tmpFile);
    p += ext;
    *out = std::move(p);
    return true;
}

bool WriteAllBytes(const std::wstring& path, const std::vector<BYTE>& data) {
    HANDLE h = CreateFileW(
        path.c_str(), GENERIC_WRITE, 0, nullptr, CREATE_ALWAYS, FILE_ATTRIBUTE_TEMPORARY, nullptr);
    if (h == INVALID_HANDLE_VALUE) {
        return false;
    }
    DWORD written = 0;
    size_t offset = 0;
    bool ok = true;
    while (offset < data.size()) {
        DWORD chunk = static_cast<DWORD>((data.size() - offset) > 0x100000u ? 0x100000u : (data.size() - offset));
        if (!WriteFile(h, data.data() + offset, chunk, &written, nullptr) || written != chunk) {
            ok = false;
            break;
        }
        offset += written;
    }
    CloseHandle(h);
    return ok;
}

HRESULT DecodePngToHBitmap(const std::wstring& pngPath, HBITMAP* phbmp, WTS_ALPHATYPE* pdwAlpha) {
    IWICImagingFactory* pFactory = nullptr;
    HRESULT hr = CoCreateInstance(
        CLSID_WICImagingFactory, nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&pFactory));
    if (FAILED(hr)) {
        return hr;
    }

    IWICBitmapDecoder* pDecoder = nullptr;
    hr = pFactory->CreateDecoderFromFilename(
        pngPath.c_str(), nullptr, GENERIC_READ, WICDecodeMetadataCacheOnDemand, &pDecoder);
    IWICBitmapFrameDecode* pFrame = nullptr;
    if (SUCCEEDED(hr)) {
        hr = pDecoder->GetFrame(0, &pFrame);
    }

    IWICFormatConverter* pConverter = nullptr;
    if (SUCCEEDED(hr)) {
        hr = pFactory->CreateFormatConverter(&pConverter);
    }
    if (SUCCEEDED(hr)) {
        hr = pConverter->Initialize(
            pFrame,
            GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            nullptr,
            0.0f,
            WICBitmapPaletteTypeCustom);
    }

    UINT width = 0, height = 0;
    if (SUCCEEDED(hr)) {
        hr = pConverter->GetSize(&width, &height);
    }
    if (SUCCEEDED(hr) && (width == 0 || height == 0 || width > 4096 || height > 4096)) {
        hr = E_FAIL;
    }

    if (SUCCEEDED(hr)) {
        BITMAPINFO bmi = {};
        bmi.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
        bmi.bmiHeader.biWidth = static_cast<LONG>(width);
        bmi.bmiHeader.biHeight = -static_cast<LONG>(height); // top-down DIB
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;

        void* pBits = nullptr;
        HDC hdc = GetDC(nullptr);
        HBITMAP hbmp = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &pBits, nullptr, 0);
        if (hdc) {
            ReleaseDC(nullptr, hdc);
        }
        if (!hbmp || !pBits) {
            if (hbmp) {
                DeleteObject(hbmp);
            }
            hr = E_OUTOFMEMORY;
        } else {
            const UINT stride = width * 4;
            const UINT bufSize = stride * height;
            hr = pConverter->CopyPixels(nullptr, stride, bufSize, static_cast<BYTE*>(pBits));
            if (SUCCEEDED(hr)) {
                *phbmp = hbmp;
                if (pdwAlpha) {
                    *pdwAlpha = WTSAT_ARGB;
                }
            } else {
                DeleteObject(hbmp);
            }
        }
    }

    if (pConverter) pConverter->Release();
    if (pFrame) pFrame->Release();
    if (pDecoder) pDecoder->Release();
    if (pFactory) pFactory->Release();
    return hr;
}

LONG SetRegString(HKEY root, const std::wstring& subkey, const wchar_t* valueName, const wchar_t* data) {
    HKEY hKey = nullptr;
    LONG rc = RegCreateKeyExW(
        root, subkey.c_str(), 0, nullptr, REG_OPTION_NON_VOLATILE, KEY_WRITE, nullptr, &hKey, nullptr);
    if (rc == ERROR_SUCCESS) {
        const DWORD bytes = static_cast<DWORD>((wcslen(data) + 1) * sizeof(wchar_t));
        rc = RegSetValueExW(hKey, valueName, 0, REG_SZ, reinterpret_cast<const BYTE*>(data), bytes);
        RegCloseKey(hKey);
    }
    return rc;
}

class CoreeThumbProvider final : public IInitializeWithStream, public IThumbnailProvider {
public:
    CoreeThumbProvider() : refCount_(1) {
        g_cDllRef.fetch_add(1, std::memory_order_relaxed);
    }

    // IUnknown
    IFACEMETHODIMP QueryInterface(REFIID riid, void** ppv) override {
        if (!ppv) return E_POINTER;
        *ppv = nullptr;
        if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_IInitializeWithStream)) {
            *ppv = static_cast<IInitializeWithStream*>(this);
        } else if (IsEqualIID(riid, IID_IThumbnailProvider)) {
            *ppv = static_cast<IThumbnailProvider*>(this);
        } else {
            return E_NOINTERFACE;
        }
        AddRef();
        return S_OK;
    }

    IFACEMETHODIMP_(ULONG) AddRef() override {
        return static_cast<ULONG>(refCount_.fetch_add(1, std::memory_order_relaxed) + 1);
    }

    IFACEMETHODIMP_(ULONG) Release() override {
        const LONG cRef = refCount_.fetch_sub(1, std::memory_order_acq_rel) - 1;
        if (cRef == 0) {
            delete this;
        }
        return static_cast<ULONG>(cRef);
    }

    // IInitializeWithStream
    IFACEMETHODIMP Initialize(IStream* pstream, DWORD /*grfMode*/) override {
        if (!pstream) return E_INVALIDARG;
        streamBytes_.clear();

        LARGE_INTEGER zero = {};
        pstream->Seek(zero, STREAM_SEEK_SET, nullptr);

        BYTE buf[65536];
        ULONG cbRead = 0;
        for (;;) {
            HRESULT hr = pstream->Read(buf, sizeof(buf), &cbRead);
            if (FAILED(hr)) {
                streamBytes_.clear();
                return hr;
            }
            if (cbRead == 0) break;
            if (streamBytes_.size() + cbRead > kMaxStreamBytes) {
                streamBytes_.clear();
                return E_FAIL;
            }
            streamBytes_.insert(streamBytes_.end(), buf, buf + cbRead);
            if (cbRead < sizeof(buf)) break;
        }
        return streamBytes_.empty() ? E_FAIL : S_OK;
    }

    // IThumbnailProvider
    IFACEMETHODIMP GetThumbnail(UINT cx, HBITMAP* phbmp, WTS_ALPHATYPE* pdwAlpha) override {
        if (!phbmp || !pdwAlpha) return E_POINTER;
        *phbmp = nullptr;
        *pdwAlpha = WTSAT_UNKNOWN;
        if (streamBytes_.empty() || cx == 0 || cx > 4096) {
            return E_FAIL;
        }

        std::wstring cliExe = SiblingExePath(L"coree.com");
        if (cliExe.empty()) {
            cliExe = SiblingExePath(L"coree.exe");
        }
        if (cliExe.empty()) {
            return E_FAIL;
        }

        std::wstring inPath, outPath;
        if (!MakeTempFilePath(L"crin", L".bin", &inPath) ||
            !MakeTempFilePath(L"crout", L".png", &outPath)) {
            if (!inPath.empty()) DeleteFileW(inPath.c_str());
            return E_FAIL;
        }

        HRESULT hr = E_FAIL;
        if (WriteAllBytes(inPath, streamBytes_)) {
            wchar_t sizeBuf[32] = {};
            swprintf_s(sizeBuf, L"%u", cx);
            std::wstring cmdLine =
                L"\"" + cliExe + L"\" thumbnail \"" + inPath + L"\" \"" + outPath + L"\" --size " + sizeBuf;

            STARTUPINFOW si = {};
            si.cb = sizeof(si);
            si.dwFlags = STARTF_USESHOWWINDOW;
            si.wShowWindow = SW_HIDE;
            PROCESS_INFORMATION pi = {};
            std::vector<wchar_t> cmdMut(cmdLine.begin(), cmdLine.end());
            cmdMut.push_back(L'\0');

            if (CreateProcessW(
                    cliExe.c_str(), cmdMut.data(), nullptr, nullptr, FALSE,
                    CREATE_NO_WINDOW, nullptr, nullptr, &si, &pi)) {
                DWORD waitRes = WaitForSingleObject(pi.hProcess, kCliTimeoutMs);
                DWORD exitCode = 1;
                if (waitRes == WAIT_OBJECT_0) {
                    GetExitCodeProcess(pi.hProcess, &exitCode);
                } else {
                    TerminateProcess(pi.hProcess, 1);
                }
                CloseHandle(pi.hThread);
                CloseHandle(pi.hProcess);
                if (waitRes == WAIT_OBJECT_0 && exitCode == 0) {
                    hr = DecodePngToHBitmap(outPath, phbmp, pdwAlpha);
                }
            }
        }

        DeleteFileW(inPath.c_str());
        DeleteFileW(outPath.c_str());
        return hr;
    }

private:
    ~CoreeThumbProvider() {
        g_cDllRef.fetch_sub(1, std::memory_order_relaxed);
    }

    std::atomic<LONG> refCount_;
    std::vector<BYTE> streamBytes_;
};

class CoreeClassFactory final : public IClassFactory {
public:
    CoreeClassFactory() : refCount_(1) {
        g_cDllRef.fetch_add(1, std::memory_order_relaxed);
    }

    IFACEMETHODIMP QueryInterface(REFIID riid, void** ppv) override {
        if (!ppv) return E_POINTER;
        *ppv = nullptr;
        if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_IClassFactory)) {
            *ppv = static_cast<IClassFactory*>(this);
            AddRef();
            return S_OK;
        }
        return E_NOINTERFACE;
    }

    IFACEMETHODIMP_(ULONG) AddRef() override {
        return static_cast<ULONG>(refCount_.fetch_add(1, std::memory_order_relaxed) + 1);
    }

    IFACEMETHODIMP_(ULONG) Release() override {
        const LONG cRef = refCount_.fetch_sub(1, std::memory_order_acq_rel) - 1;
        if (cRef == 0) {
            delete this;
        }
        return static_cast<ULONG>(cRef);
    }

    IFACEMETHODIMP CreateInstance(IUnknown* pUnkOuter, REFIID riid, void** ppv) override {
        if (pUnkOuter != nullptr) return CLASS_E_NOAGGREGATION;
        auto* pObj = new (std::nothrow) CoreeThumbProvider();
        if (!pObj) return E_OUTOFMEMORY;
        const HRESULT hr = pObj->QueryInterface(riid, ppv);
        pObj->Release();
        return hr;
    }

    IFACEMETHODIMP LockServer(BOOL fLock) override {
        if (fLock) {
            g_cDllRef.fetch_add(1, std::memory_order_relaxed);
        } else {
            g_cDllRef.fetch_sub(1, std::memory_order_relaxed);
        }
        return S_OK;
    }

private:
    ~CoreeClassFactory() {
        g_cDllRef.fetch_sub(1, std::memory_order_relaxed);
    }

    std::atomic<LONG> refCount_;
};

} // namespace

BOOL APIENTRY DllMain(HMODULE hModule, DWORD ul_reason_for_call, LPVOID /*lpReserved*/) {
    if (ul_reason_for_call == DLL_PROCESS_ATTACH) {
        g_hModule = hModule;
        DisableThreadLibraryCalls(hModule);
    }
    return TRUE;
}

STDAPI DllGetClassObject(REFCLSID rclsid, REFIID riid, void** ppv) {
    if (!ppv) return E_POINTER;
    *ppv = nullptr;
    if (!IsEqualCLSID(rclsid, CLSID_CoreeThumbProvider)) {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    auto* pFactory = new (std::nothrow) CoreeClassFactory();
    if (!pFactory) return E_OUTOFMEMORY;
    const HRESULT hr = pFactory->QueryInterface(riid, ppv);
    pFactory->Release();
    return hr;
}

STDAPI DllCanUnloadNow() {
    return g_cDllRef.load(std::memory_order_relaxed) > 0 ? S_FALSE : S_OK;
}

STDAPI DllRegisterServer() {
    wchar_t dllPath[MAX_PATH] = {};
    if (GetModuleFileNameW(g_hModule, dllPath, MAX_PATH) == 0) {
        return HRESULT_FROM_WIN32(GetLastError());
    }
    const std::wstring clsidKey = std::wstring(L"Software\\Classes\\CLSID\\") + kClsidStr;
    if (SetRegString(HKEY_CURRENT_USER, clsidKey, nullptr, kProviderDesc) != ERROR_SUCCESS ||
        SetRegString(HKEY_CURRENT_USER, clsidKey + L"\\InprocServer32", nullptr, dllPath) != ERROR_SUCCESS ||
        SetRegString(HKEY_CURRENT_USER, clsidKey + L"\\InprocServer32", L"ThreadingModel", L"Apartment") != ERROR_SUCCESS) {
        return E_FAIL;
    }
    for (const wchar_t* ext : kThumbExtensions) {
        const std::wstring shellexKey =
            std::wstring(L"Software\\Classes\\") + ext + L"\\ShellEx\\" + kThumbProviderIidStr;
        if (SetRegString(HKEY_CURRENT_USER, shellexKey, nullptr, kClsidStr) != ERROR_SUCCESS) {
            return E_FAIL;
        }
    }
    SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, nullptr, nullptr);
    return S_OK;
}

STDAPI DllUnregisterServer() {
    const std::wstring clsidKey = std::wstring(L"Software\\Classes\\CLSID\\") + kClsidStr;
    RegDeleteTreeW(HKEY_CURRENT_USER, clsidKey.c_str());
    for (const wchar_t* ext : kThumbExtensions) {
        const std::wstring shellexKey =
            std::wstring(L"Software\\Classes\\") + ext + L"\\ShellEx\\" + kThumbProviderIidStr;
        RegDeleteTreeW(HKEY_CURRENT_USER, shellexKey.c_str());
    }
    SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, nullptr, nullptr);
    return S_OK;
}
