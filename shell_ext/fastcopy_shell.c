#define WIN32_LEAN_AND_MEAN
#define COBJMACROS
#include <windows.h>
#include <shlobj.h>
#include <shellapi.h>
#include <strsafe.h>
#include <ole2.h>

#pragma comment(lib, "ole32.lib")
#pragma comment(lib, "shell32.lib")
#pragma comment(lib, "user32.lib")
#pragma comment(lib, "gdi32.lib")
#pragma comment(lib, "advapi32.lib")
#pragma comment(lib, "uuid.lib")

enum { BMP_APP = 0, BMP_CUT, BMP_COPY, BMP_DELETE, BMP_SIZE, BMP_PATH, BMP_SETTINGS, BMP_COUNT };
enum { CMD_CUT = 0, CMD_COPY, CMD_DELETE, CMD_SYMLINK, CMD_HARDLINK, CMD_OPEN, CMD_SHOW, CMD_SIZE, CMD_COPYPATH, CMD_SETTINGS, CMD_COUNT };

static const GUID CLSID_FastCopyMenu = {0xB3E8D47A, 0x6C1F, 0x4A92, {0x9E, 0x05, 0x8F, 0x4C, 0x2B, 0x17, 0xA6, 0xD0}};
static const WCHAR kCascadeZh[] = {0x5FEB, 0x901F, 0x590D, 0x5236, 0};

static HINSTANCE g_instance;
static LONG g_objs;
static LONG g_locks;

struct Handler {
	IContextMenu menu;
	IShellExtInit init;
	LONG ref;
	UINT nfiles;
	WCHAR first[32768];
	HBITMAP bmp[BMP_COUNT];
};

static HRESULT Handler_QueryInterface(struct Handler *h, REFIID riid, void **ppv);
static ULONG Handler_AddRef(struct Handler *h);
static ULONG Handler_Release(struct Handler *h);
static void handler_clear_bitmaps(struct Handler *h);
static HRESULT launch(struct Handler *h, UINT id);
static int exe_path(WCHAR *out, UINT cap);
static int menu_has_cascade(HMENU menu);
static int is_single_link(struct Handler *h);
static void read_label(const WCHAR *sub, WCHAR *out, UINT cap, const WCHAR *fallback);
static HBITMAP load_icon_bitmap(const WCHAR *name);
static void insert_cmd(HMENU sub, UINT pos, UINT id, const WCHAR *subkey, const WCHAR *fallback, HBITMAP bmp);
static void strip_amp(WCHAR *s);

static HRESULT STDMETHODCALLTYPE Menu_QueryInterface(IContextMenu *this, REFIID riid, void **ppv){
	return Handler_QueryInterface(CONTAINING_RECORD(this, struct Handler, menu), riid, ppv);
}

static ULONG STDMETHODCALLTYPE Menu_AddRef(IContextMenu *this){
	return Handler_AddRef(CONTAINING_RECORD(this, struct Handler, menu));
}

static ULONG STDMETHODCALLTYPE Menu_Release(IContextMenu *this){
	return Handler_Release(CONTAINING_RECORD(this, struct Handler, menu));
}

static HRESULT STDMETHODCALLTYPE Menu_QueryContextMenu(IContextMenu *this, HMENU hmenu, UINT indexMenu, UINT idCmdFirst, UINT idCmdLast, UINT uFlags){
	struct Handler *h = CONTAINING_RECORD(this, struct Handler, menu);
	HMENU sub;
	WCHAR label[256];
	(void)idCmdLast;
	if(uFlags & CMF_DEFAULTONLY) return MAKE_HRESULT(SEVERITY_SUCCESS, 0, 0);
	if(!hmenu) return MAKE_HRESULT(SEVERITY_SUCCESS, 0, 0);
	if(menu_has_cascade(hmenu)) return MAKE_HRESULT(SEVERITY_SUCCESS, 0, 0);
	handler_clear_bitmaps(h);
	h->bmp[BMP_APP] = load_icon_bitmap(L"app.ico");
	h->bmp[BMP_CUT] = load_icon_bitmap(L"cut.ico");
	h->bmp[BMP_COPY] = load_icon_bitmap(L"copy.ico");
	h->bmp[BMP_DELETE] = load_icon_bitmap(L"delete.ico");
	h->bmp[BMP_SIZE] = load_icon_bitmap(L"size.ico");
	h->bmp[BMP_PATH] = load_icon_bitmap(L"path.ico");
	h->bmp[BMP_SETTINGS] = load_icon_bitmap(L"settings.ico");
	sub = CreatePopupMenu();
	if(!sub) return E_OUTOFMEMORY;
	{
		UINT pos = 0;
		insert_cmd(sub, pos++, idCmdFirst + CMD_CUT, L"shell\\1cut", L"Quick Cut", h->bmp[BMP_CUT]);
		insert_cmd(sub, pos++, idCmdFirst + CMD_COPY, L"shell\\2copy", L"Quick Copy", h->bmp[BMP_COPY]);
		insert_cmd(sub, pos++, idCmdFirst + CMD_DELETE, L"shell\\3delete", L"Quick Delete", h->bmp[BMP_DELETE]);
		insert_cmd(sub, pos++, idCmdFirst + CMD_SYMLINK, L"shell\\4symlink", L"Copy as symbolic link", h->bmp[BMP_COPY]);
		insert_cmd(sub, pos++, idCmdFirst + CMD_HARDLINK, L"shell\\5hardlink", L"Copy as hard link", h->bmp[BMP_COPY]);
		if(is_single_link(h)){
			insert_cmd(sub, pos++, idCmdFirst + CMD_OPEN, L"shell\\6open", L"Open link target", h->bmp[BMP_APP]);
			insert_cmd(sub, pos++, idCmdFirst + CMD_SHOW, L"shell\\6path", L"View source path", h->bmp[BMP_APP]);
		}
		insert_cmd(sub, pos++, idCmdFirst + CMD_SIZE, L"shell\\7size", L"Folder size", h->bmp[BMP_SIZE]);
		insert_cmd(sub, pos++, idCmdFirst + CMD_COPYPATH, L"shell\\8copypath", L"Copy paths", h->bmp[BMP_PATH]);
		InsertMenuW(sub, pos++, MF_BYPOSITION | MF_SEPARATOR, 0, NULL);
		insert_cmd(sub, pos++, idCmdFirst + CMD_SETTINGS, L"shell\\zsettings", L"Settings", h->bmp[BMP_SETTINGS]);
	}
	read_label(L"", label, 256, L"FastCopy");
	{
		MENUITEMINFOW mii;
		memset(&mii, 0, sizeof(mii));
		mii.cbSize = sizeof(mii);
		mii.fMask = MIIM_STRING | MIIM_SUBMENU | MIIM_BITMAP;
		mii.hSubMenu = sub;
		mii.dwTypeData = label;
		mii.cch = (UINT)wcslen(label);
		mii.hbmpItem = h->bmp[BMP_APP];
		if(!InsertMenuItemW(hmenu, indexMenu, TRUE, &mii)){
			DestroyMenu(sub);
			return E_FAIL;
		}
	}
	return MAKE_HRESULT(SEVERITY_SUCCESS, 0, CMD_COUNT);
}

static HRESULT STDMETHODCALLTYPE Menu_InvokeCommand(IContextMenu *this, CMINVOKECOMMANDINFO *pici){
	struct Handler *h = CONTAINING_RECORD(this, struct Handler, menu);
	UINT id;
	if(!pici) return E_INVALIDARG;
	if(!IS_INTRESOURCE(pici->lpVerb)) return E_FAIL;
	id = LOWORD((UINT_PTR)pici->lpVerb);
	return launch(h, id);
}

static HRESULT STDMETHODCALLTYPE Menu_GetCommandString(IContextMenu *this, UINT_PTR idCmd, UINT uType, UINT *pReserved, CHAR *pszName, UINT cchMax){
	(void)this;
	(void)idCmd;
	(void)uType;
	(void)pReserved;
	(void)pszName;
	(void)cchMax;
	return E_NOTIMPL;
}

static HRESULT STDMETHODCALLTYPE Init_QueryInterface(IShellExtInit *this, REFIID riid, void **ppv){
	return Handler_QueryInterface(CONTAINING_RECORD(this, struct Handler, init), riid, ppv);
}

static ULONG STDMETHODCALLTYPE Init_AddRef(IShellExtInit *this){
	return Handler_AddRef(CONTAINING_RECORD(this, struct Handler, init));
}

static ULONG STDMETHODCALLTYPE Init_Release(IShellExtInit *this){
	return Handler_Release(CONTAINING_RECORD(this, struct Handler, init));
}

static HRESULT STDMETHODCALLTYPE Init_Initialize(IShellExtInit *this, LPCITEMIDLIST pidlFolder, IDataObject *pdtobj, HKEY hkeyProgID){
	struct Handler *h = CONTAINING_RECORD(this, struct Handler, init);
	FORMATETC fe;
	STGMEDIUM stg;
	UINT n_drop = 0, n_ida = 0;
	WCHAR drop_first[32768], ida_first[32768], folder[32768];
	(void)hkeyProgID;
	h->nfiles = 0;
	h->first[0] = 0;
	drop_first[0] = 0;
	ida_first[0] = 0;
	if(pdtobj){
		memset(&fe, 0, sizeof(fe));
		fe.dwAspect = DVASPECT_CONTENT;
		fe.lindex = -1;
		fe.tymed = TYMED_HGLOBAL;
		fe.cfFormat = CF_HDROP;
		if(SUCCEEDED(IDataObject_GetData(pdtobj, &fe, &stg))){
			n_drop = DragQueryFileW((HDROP)stg.hGlobal, 0xFFFFFFFF, NULL, 0);
			if(n_drop > 0) DragQueryFileW((HDROP)stg.hGlobal, 0, drop_first, 32768);
			ReleaseStgMedium(&stg);
		}
		fe.cfFormat = (CLIPFORMAT)RegisterClipboardFormatW(CFSTR_SHELLIDLIST);
		if(SUCCEEDED(IDataObject_GetData(pdtobj, &fe, &stg))){
			CIDA *cida = (CIDA *)GlobalLock(stg.hGlobal);
			if(cida){
				n_ida = cida->cidl;
				if(n_ida > 0){
					LPITEMIDLIST abs = ILCombine((LPCITEMIDLIST)((BYTE *)cida + cida->aoffset[0]), (LPCITEMIDLIST)((BYTE *)cida + cida->aoffset[1]));
					if(abs){
						SHGetPathFromIDListW(abs, ida_first);
						ILFree(abs);
					}
				}
				GlobalUnlock(stg.hGlobal);
			}
			ReleaseStgMedium(&stg);
		}
		if(n_ida > n_drop){
			h->nfiles = n_ida;
			lstrcpynW(h->first, ida_first, 32768);
		}
		else {
			h->nfiles = n_drop;
			lstrcpynW(h->first, drop_first, 32768);
		}
	}
	if(pidlFolder){
		folder[0] = 0;
		if(SHGetPathFromIDListW(pidlFolder, folder) && folder[0] && !h->first[0]) lstrcpynW(h->first, folder, 32768);
	}
	return S_OK;
}

static IContextMenuVtbl g_menu_vtbl = {
	Menu_QueryInterface,
	Menu_AddRef,
	Menu_Release,
	Menu_QueryContextMenu,
	Menu_InvokeCommand,
	Menu_GetCommandString
};

static IShellExtInitVtbl g_init_vtbl = {
	Init_QueryInterface,
	Init_AddRef,
	Init_Release,
	Init_Initialize
};

static HRESULT Handler_QueryInterface(struct Handler *h, REFIID riid, void **ppv){
	if(!ppv) return E_POINTER;
	*ppv = NULL;
	if(IsEqualIID(riid, &IID_IUnknown) || IsEqualIID(riid, &IID_IContextMenu)) *ppv = &h->menu;
	else if(IsEqualIID(riid, &IID_IShellExtInit)) *ppv = &h->init;
	else return E_NOINTERFACE;
	Handler_AddRef(h);
	return S_OK;
}

static ULONG Handler_AddRef(struct Handler *h){
	return (ULONG)InterlockedIncrement(&h->ref);
}

static ULONG Handler_Release(struct Handler *h){
	LONG n = InterlockedDecrement(&h->ref);
	if(n) return (ULONG)n;
	handler_clear_bitmaps(h);
	CoTaskMemFree(h);
	InterlockedDecrement(&g_objs);
	return 0;
}

static void handler_clear_bitmaps(struct Handler *h){
	UINT i;
	for(i = 0; i < BMP_COUNT; i++){
		if(h->bmp[i]){
			DeleteObject(h->bmp[i]);
			h->bmp[i] = NULL;
		}
	}
}

static HRESULT STDMETHODCALLTYPE Factory_QueryInterface(IClassFactory *this, REFIID riid, void **ppv);
static ULONG STDMETHODCALLTYPE Factory_AddRef(IClassFactory *this);
static ULONG STDMETHODCALLTYPE Factory_Release(IClassFactory *this);
static HRESULT STDMETHODCALLTYPE Factory_CreateInstance(IClassFactory *this, IUnknown *outer, REFIID riid, void **ppv);
static HRESULT STDMETHODCALLTYPE Factory_LockServer(IClassFactory *this, BOOL flock);

static IClassFactoryVtbl g_factory_vtbl = {
	Factory_QueryInterface,
	Factory_AddRef,
	Factory_Release,
	Factory_CreateInstance,
	Factory_LockServer
};

static IClassFactory g_factory = {&g_factory_vtbl};

static HRESULT STDMETHODCALLTYPE Factory_QueryInterface(IClassFactory *this, REFIID riid, void **ppv){
	if(!ppv) return E_POINTER;
	*ppv = NULL;
	if(IsEqualIID(riid, &IID_IUnknown) || IsEqualIID(riid, &IID_IClassFactory)){
		*ppv = this;
		return S_OK;
	}
	return E_NOINTERFACE;
}

static ULONG STDMETHODCALLTYPE Factory_AddRef(IClassFactory *this){
	(void)this;
	return 2;
}

static ULONG STDMETHODCALLTYPE Factory_Release(IClassFactory *this){
	(void)this;
	return 1;
}

static HRESULT STDMETHODCALLTYPE Factory_CreateInstance(IClassFactory *this, IUnknown *outer, REFIID riid, void **ppv){
	struct Handler *h;
	HRESULT hr;
	(void)this;
	if(outer) return CLASS_E_NOAGGREGATION;
	h = (struct Handler *)CoTaskMemAlloc(sizeof(*h));
	if(!h) return E_OUTOFMEMORY;
	memset(h, 0, sizeof(*h));
	h->menu.lpVtbl = &g_menu_vtbl;
	h->init.lpVtbl = &g_init_vtbl;
	h->ref = 1;
	InterlockedIncrement(&g_objs);
	hr = Handler_QueryInterface(h, riid, ppv);
	Handler_Release(h);
	return hr;
}

static HRESULT STDMETHODCALLTYPE Factory_LockServer(IClassFactory *this, BOOL flock){
	(void)this;
	if(flock) InterlockedIncrement(&g_locks);
	else InterlockedDecrement(&g_locks);
	return S_OK;
}

static HRESULT launch(struct Handler *h, UINT id){
	WCHAR exe[32768];
	WCHAR cmd[65536];
	const WCHAR *flag;
	STARTUPINFOW si;
	PROCESS_INFORMATION pi;
	if(!exe_path(exe, 32768)) return E_FAIL;
	switch(id){
	case CMD_CUT: flag = L"--shell-cut"; break;
	case CMD_COPY: flag = L"--shell-copy"; break;
	case CMD_DELETE: flag = L"--shell-delete"; break;
	case CMD_SYMLINK: flag = L"--shell-copy-symlink"; break;
	case CMD_HARDLINK: flag = L"--shell-copy-hardlink"; break;
	case CMD_OPEN: flag = L"--shell-open-target"; break;
	case CMD_SHOW: flag = L"--shell-show-source"; break;
	case CMD_SIZE: flag = L"--shell-size"; break;
	case CMD_COPYPATH: flag = L"--shell-copy-path"; break;
	case CMD_SETTINGS: flag = L"--settings"; break;
	default: return E_INVALIDARG;
	}
	if(id == CMD_SETTINGS || !h->first[0]){
		if(FAILED(StringCchPrintfW(cmd, 65536, L"\"%s\" %s", exe, flag))) return E_FAIL;
	}
	else {
		if(FAILED(StringCchPrintfW(cmd, 65536, L"\"%s\" %s \"%s\"", exe, flag, h->first))) return E_FAIL;
	}
	memset(&si, 0, sizeof(si));
	si.cb = sizeof(si);
	if(!CreateProcessW(NULL, cmd, NULL, NULL, FALSE, CREATE_NO_WINDOW, NULL, NULL, &si, &pi)) return HRESULT_FROM_WIN32(GetLastError());
	CloseHandle(pi.hThread);
	CloseHandle(pi.hProcess);
	return S_OK;
}

static int exe_path(WCHAR *out, UINT cap){
	WCHAR path[32768];
	DWORD n;
	WCHAR *slash;
	n = GetModuleFileNameW(g_instance, path, 32768);
	if(n == 0 || n >= 32768) return 0;
	slash = wcsrchr(path, L'\\');
	if(!slash) return 0;
	slash[1] = 0;
	if(FAILED(StringCchCatW(path, 32768, L"fastcopy.exe"))) return 0;
	if(GetFileAttributesW(path) == INVALID_FILE_ATTRIBUTES) return 0;
	lstrcpynW(out, path, (int)cap);
	return 1;
}

static void strip_amp(WCHAR *s){
	WCHAR *r = s, *w = s;
	while(*r){
		if(*r == L'&') r++;
		else *w++ = *r++;
	}
	*w = 0;
}

static int reg_mui(HKEY hive, const WCHAR *path, WCHAR *out, UINT cap){
	HKEY key;
	DWORD n, type;
	if(RegOpenKeyExW(hive, path, 0, KEY_READ, &key) != 0) return 0;
	n = cap * sizeof(WCHAR);
	if(RegQueryValueExW(key, L"MUIVerb", NULL, &type, (BYTE *)out, &n) != 0 || type != REG_SZ) out[0] = 0;
	RegCloseKey(key);
	return out[0] != 0;
}

static int is_single_link(struct Handler *h){
	const WCHAR *dot;
	DWORD attr;
	if(h->nfiles != 1 || !h->first[0]) return 0;
	dot = wcsrchr(h->first, L'.');
	if(dot && !lstrcmpiW(dot, L".lnk")) return 1;
	attr = GetFileAttributesW(h->first);
	return attr != INVALID_FILE_ATTRIBUTES && (attr & FILE_ATTRIBUTE_REPARSE_POINT);
}

static int menu_has_cascade(HMENU menu){
	WCHAR want[256];
	WCHAR text[512];
	MENUITEMINFOW info;
	int n, i;
	read_label(L"", want, 256, L"FastCopy");
	n = GetMenuItemCount(menu);
	for(i = 0; i < n; i++){
		text[0] = 0;
		memset(&info, 0, sizeof(info));
		info.cbSize = sizeof(info);
		info.fMask = MIIM_STRING | MIIM_SUBMENU;
		info.dwTypeData = text;
		info.cch = 511;
		if(!GetMenuItemInfoW(menu, (UINT)i, TRUE, &info) || !text[0]) continue;
		strip_amp(text);
		if(!lstrcmpiW(text, want) || !lstrcmpiW(text, L"FastCopy") || !lstrcmpiW(text, kCascadeZh)) return 1;
	}
	return 0;
}

static void read_label(const WCHAR *sub, WCHAR *out, UINT cap, const WCHAR *fallback){
	WCHAR path[512];
	out[0] = 0;
	if(sub && sub[0]){
		if(FAILED(StringCchPrintfW(path, 512, L"Software\\FastCopyMenu\\%s", sub))){
			lstrcpynW(out, fallback, (int)cap);
			return;
		}
	}
	else StringCchCopyW(path, 512, L"Software\\FastCopyMenu");
	if(!reg_mui(HKEY_CURRENT_USER, path, out, cap) && sub && sub[0]){
		if(SUCCEEDED(StringCchPrintfW(path, 512, L"SOFTWARE\\FastCopyMenu\\%s", sub))) reg_mui(HKEY_LOCAL_MACHINE, path, out, cap);
	}
	else if(!out[0]) reg_mui(HKEY_LOCAL_MACHINE, L"SOFTWARE\\FastCopyMenu", out, cap);
	if(!out[0]){
		if(sub && sub[0]){
			if(FAILED(StringCchPrintfW(path, 512, L"Software\\Classes\\*\\shell\\FastCopyRust\\%s", sub))){
				lstrcpynW(out, fallback, (int)cap);
				return;
			}
		}
		else StringCchCopyW(path, 512, L"Software\\Classes\\*\\shell\\FastCopyRust");
		if(!reg_mui(HKEY_CURRENT_USER, path, out, cap) && (!sub || !sub[0])){
			reg_mui(HKEY_LOCAL_MACHINE, L"SOFTWARE\\Classes\\*\\shell\\FastCopyRust", out, cap);
		}
	}
	if(!out[0]) lstrcpynW(out, fallback, (int)cap);
}

static HBITMAP load_icon_bitmap(const WCHAR *name){
	WCHAR dir[32768];
	WCHAR path[32768];
	HICON icon;
	HDC screen, mem;
	BITMAPINFO bi;
	HBITMAP bmp, old;
	void *bits;
	DWORD n;
	n = GetEnvironmentVariableW(L"LOCALAPPDATA", dir, 32768);
	if(n == 0 || n >= 32768) return NULL;
	if(FAILED(StringCchPrintfW(path, 32768, L"%s\\FastCopy\\icons\\%s", dir, name))) return NULL;
	icon = (HICON)LoadImageW(NULL, path, IMAGE_ICON, 16, 16, LR_LOADFROMFILE);
	if(!icon) return NULL;
	memset(&bi, 0, sizeof(bi));
	bi.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
	bi.bmiHeader.biWidth = 16;
	bi.bmiHeader.biHeight = -16;
	bi.bmiHeader.biPlanes = 1;
	bi.bmiHeader.biBitCount = 32;
	bi.bmiHeader.biCompression = BI_RGB;
	screen = GetDC(NULL);
	mem = CreateCompatibleDC(screen);
	bmp = CreateDIBSection(mem, &bi, DIB_RGB_COLORS, &bits, NULL, 0);
	if(bmp){
		old = (HBITMAP)SelectObject(mem, bmp);
		DrawIconEx(mem, 0, 0, icon, 16, 16, 0, NULL, DI_NORMAL);
		SelectObject(mem, old);
	}
	DeleteDC(mem);
	ReleaseDC(NULL, screen);
	DestroyIcon(icon);
	return bmp;
}

static void insert_cmd(HMENU sub, UINT pos, UINT id, const WCHAR *subkey, const WCHAR *fallback, HBITMAP bmp){
	WCHAR label[256];
	MENUITEMINFOW mii;
	read_label(subkey, label, 256, fallback);
	memset(&mii, 0, sizeof(mii));
	mii.cbSize = sizeof(mii);
	mii.fMask = MIIM_STRING | MIIM_ID | MIIM_BITMAP;
	mii.wID = id;
	mii.dwTypeData = label;
	mii.cch = (UINT)wcslen(label);
	mii.hbmpItem = bmp;
	InsertMenuItemW(sub, pos, TRUE, &mii);
}

BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, void *reserved){
	(void)reserved;
	if(reason == DLL_PROCESS_ATTACH){
		g_instance = instance;
		DisableThreadLibraryCalls(instance);
	}
	return TRUE;
}

STDAPI DllGetClassObject(REFCLSID rclsid, REFIID riid, void **ppv){
	if(!IsEqualCLSID(rclsid, &CLSID_FastCopyMenu)) return CLASS_E_CLASSNOTAVAILABLE;
	return g_factory.lpVtbl->QueryInterface(&g_factory, riid, ppv);
}

STDAPI DllCanUnloadNow(void){
	if(g_objs == 0 && g_locks == 0) return S_OK;
	return S_FALSE;
}
