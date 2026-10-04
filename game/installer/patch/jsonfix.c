/* jsonfix.c - Store JSON response fixer.
 *
 * Minimal JSON parser + DOM walker. Finds item objects (anything with a
 * name-like string field plus an AvatarItemType/OutfitType numeric field)
 * and ORs in the category bit the 2025 client expects.
 */
#include "jsonfix.h"
#include <stdlib.h>
#include <string.h>
#include <ctype.h>
#include <stdio.h>

/* ---------------- DOM ---------------- */
typedef enum { J_NULL, J_BOOL, J_NUM, J_STR, J_ARR, J_OBJ } jtype_t;

typedef struct jval jval_t;
typedef struct {
    char  *key;
    jval_t *val;
} jpair_t;

struct jval {
    jtype_t type;
    union {
        int      b;                 /* bool */
        double   num;               /* number */
        struct { char *s; size_t n; } str;  /* string (unescaped) */
        struct { jval_t **items; size_t n, cap; } arr;
        struct { jpair_t *pairs; size_t n, cap; } obj;
    } u;
};

static jval_t *jnew(jtype_t t) {
    jval_t *v = (jval_t *)calloc(1, sizeof(*v));
    if (v) v->type = t;
    return v;
}

static void jfree(jval_t *v) {
    size_t i;
    if (!v) return;
    switch (v->type) {
    case J_STR: free(v->u.str.s); break;
    case J_ARR:
        for (i = 0; i < v->u.arr.n; i++) jfree(v->u.arr.items[i]);
        free(v->u.arr.items);
        break;
    case J_OBJ:
        for (i = 0; i < v->u.obj.n; i++) {
            free(v->u.obj.pairs[i].key);
            jfree(v->u.obj.pairs[i].val);
        }
        free(v->u.obj.pairs);
        break;
    default: break;
    }
    free(v);
}

static int jarr_add(jval_t *arr, jval_t *v) {
    if (arr->u.arr.n == arr->u.arr.cap) {
        size_t nc = arr->u.arr.cap ? arr->u.arr.cap * 2 : 8;
        jval_t **ni = (jval_t **)realloc(arr->u.arr.items, nc * sizeof(*ni));
        if (!ni) return -1;
        arr->u.arr.items = ni;
        arr->u.arr.cap = nc;
    }
    arr->u.arr.items[arr->u.arr.n++] = v;
    return 0;
}

static int jobj_add(jval_t *obj, char *key, jval_t *v) {
    if (obj->u.obj.n == obj->u.obj.cap) {
        size_t nc = obj->u.obj.cap ? obj->u.obj.cap * 2 : 8;
        jpair_t *np = (jpair_t *)realloc(obj->u.obj.pairs, nc * sizeof(*np));
        if (!np) return -1;
        obj->u.obj.pairs = np;
        obj->u.obj.cap = nc;
    }
    obj->u.obj.pairs[obj->u.obj.n].key = key;
    obj->u.obj.pairs[obj->u.obj.n].val = v;
    obj->u.obj.n++;
    return 0;
}

static jval_t *jobj_get(jval_t *obj, const char *key) {
    size_t i;
    if (!obj || obj->type != J_OBJ) return NULL;
    for (i = 0; i < obj->u.obj.n; i++)
        if (strcmp(obj->u.obj.pairs[i].key, key) == 0)
            return obj->u.obj.pairs[i].val;
    return NULL;
}

/* ---------------- Parser ---------------- */
typedef struct {
    const char *p;
    const char *end;
} parser_t;

static void pskip(parser_t *p) {
    while (p->p < p->end && (*p->p == ' ' || *p->p == '\t' ||
           *p->p == '\n' || *p->p == '\r')) p->p++;
}

static jval_t *pvalue(parser_t *p);

static int hexval(char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

/* Parse a JSON string, returns unescaped malloc'd buffer + length. */
static char *pstring(parser_t *p, size_t *out_n) {
    /* p->p points at opening quote */
    size_t cap = 64, n = 0;
    char *buf = (char *)malloc(cap);
    if (!buf) return NULL;
    p->p++; /* skip " */
    while (p->p < p->end) {
        char c = *p->p;
        if (c == '"') { p->p++; break; }
        if (c == '\\' && p->p + 1 < p->end) {
            p->p++;
            c = *p->p;
            switch (c) {
            case '"': case '\\': case '/': break;
            case 'b': c = '\b'; break;
            case 'f': c = '\f'; break;
            case 'n': c = '\n'; break;
            case 'r': c = '\r'; break;
            case 't': c = '\t'; break;
            case 'u': {
                /* \uXXXX -> encode as UTF-8 (BMP only is fine here) */
                int cp = 0, i;
                if (p->p + 4 >= p->end) { free(buf); return NULL; }
                for (i = 1; i <= 4; i++) {
                    int h = hexval(p->p[i]);
                    if (h < 0) { free(buf); return NULL; }
                    cp = (cp << 4) | h;
                }
                p->p += 4;
                /* UTF-8 encode */
                if (n + 4 >= cap) {
                    cap *= 2;
                    buf = (char *)realloc(buf, cap);
                    if (!buf) return NULL;
                }
                if (cp < 0x80) buf[n++] = (char)cp;
                else if (cp < 0x800) {
                    buf[n++] = (char)(0xC0 | (cp >> 6));
                    buf[n++] = (char)(0x80 | (cp & 0x3F));
                } else {
                    buf[n++] = (char)(0xE0 | (cp >> 12));
                    buf[n++] = (char)(0x80 | ((cp >> 6) & 0x3F));
                    buf[n++] = (char)(0x80 | (cp & 0x3F));
                }
                p->p++;
                continue;
            }
            default: break; /* keep char as-is */
            }
            p->p++;
        } else {
            p->p++;
        }
        if (n + 1 >= cap) {
            cap *= 2;
            buf = (char *)realloc(buf, cap);
            if (!buf) return NULL;
        }
        buf[n++] = c;
    }
    buf[n] = '\0';
    if (out_n) *out_n = n;
    return buf;
}

static jval_t *pvalue(parser_t *p) {
    jval_t *v;
    pskip(p);
    if (p->p >= p->end) return NULL;
    switch (*p->p) {
    case '{': {
        p->p++;
        v = jnew(J_OBJ);
        if (!v) return NULL;
        pskip(p);
        if (p->p < p->end && *p->p == '}') { p->p++; return v; }
        while (p->p < p->end) {
            char *key;
            size_t kn;
            jval_t *val;
            pskip(p);
            if (p->p >= p->end || *p->p != '"') { jfree(v); return NULL; }
            key = pstring(p, &kn);
            if (!key) { jfree(v); return NULL; }
            pskip(p);
            if (p->p >= p->end || *p->p != ':') { free(key); jfree(v); return NULL; }
            p->p++;
            val = pvalue(p);
            if (!val) { free(key); jfree(v); return NULL; }
            if (jobj_add(v, key, val) < 0) { free(key); jfree(v); return NULL; }
            pskip(p);
            if (p->p < p->end && *p->p == ',') { p->p++; continue; }
            if (p->p < p->end && *p->p == '}') { p->p++; break; }
            jfree(v);
            return NULL;
        }
        return v;
    }
    case '[': {
        p->p++;
        v = jnew(J_ARR);
        if (!v) return NULL;
        pskip(p);
        if (p->p < p->end && *p->p == ']') { p->p++; return v; }
        while (p->p < p->end) {
            jval_t *item = pvalue(p);
            if (!item) { jfree(v); return NULL; }
            if (jarr_add(v, item) < 0) { jfree(v); return NULL; }
            pskip(p);
            if (p->p < p->end && *p->p == ',') { p->p++; continue; }
            if (p->p < p->end && *p->p == ']') { p->p++; break; }
            jfree(v);
            return NULL;
        }
        return v;
    }
    case '"': {
        size_t n;
        char *s = pstring(p, &n);
        if (!s) return NULL;
        v = jnew(J_STR);
        if (!v) { free(s); return NULL; }
        v->u.str.s = s;
        v->u.str.n = n;
        return v;
    }
    case 't':
        if (p->end - p->p >= 4 && memcmp(p->p, "true", 4) == 0) {
            p->p += 4; v = jnew(J_BOOL); if (v) v->u.b = 1; return v;
        }
        return NULL;
    case 'f':
        if (p->end - p->p >= 5 && memcmp(p->p, "false", 5) == 0) {
            p->p += 5; v = jnew(J_BOOL); if (v) v->u.b = 0; return v;
        }
        return NULL;
    case 'n':
        if (p->end - p->p >= 4 && memcmp(p->p, "null", 4) == 0) {
            p->p += 4; return jnew(J_NULL);
        }
        return NULL;
    default: {
        /* number */
        const char *start = p->p;
        char *e;
        double d;
        if (*p->p != '-' && !isdigit((unsigned char)*p->p)) return NULL;
        d = strtod(start, &e);
        if (e == start) return NULL;
        p->p = e;
        v = jnew(J_NUM);
        if (v) v->u.num = d;
        return v;
    }
    }
}

/* ---------------- Serializer ---------------- */
typedef struct {
    char *buf;
    size_t n, cap;
} writer_t;

static int wput(writer_t *w, const char *s, size_t n) {
    if (w->n + n + 1 > w->cap) {
        size_t nc = w->cap ? w->cap * 2 : 256;
        while (nc < w->n + n + 1) nc *= 2;
        w->buf = (char *)realloc(w->buf, nc);
        if (!w->buf) return -1;
        w->cap = nc;
    }
    memcpy(w->buf + w->n, s, n);
    w->n += n;
    return 0;
}

static int wch(writer_t *w, char c) { return wput(w, &c, 1); }

static int wstr(writer_t *w, const char *s, size_t n) {
    size_t i;
    if (wch(w, '"') < 0) return -1;
    for (i = 0; i < n; i++) {
        unsigned char c = (unsigned char)s[i];
        switch (c) {
        case '"':  if (wput(w, "\\\"", 2) < 0) return -1; break;
        case '\\': if (wput(w, "\\\\", 2) < 0) return -1; break;
        case '\b': if (wput(w, "\\b", 2) < 0) return -1; break;
        case '\f': if (wput(w, "\\f", 2) < 0) return -1; break;
        case '\n': if (wput(w, "\\n", 2) < 0) return -1; break;
        case '\r': if (wput(w, "\\r", 2) < 0) return -1; break;
        case '\t': if (wput(w, "\\t", 2) < 0) return -1; break;
        default:
            if (c < 0x20) {
                char esc[7];
                snprintf(esc, sizeof(esc), "\\u%04x", c);
                if (wput(w, esc, 6) < 0) return -1;
            } else if (wch(w, (char)c) < 0) return -1;
        }
    }
    return wch(w, '"');
}

static int wval(writer_t *w, jval_t *v) {
    size_t i;
    char numbuf[64];
    switch (v->type) {
    case J_NULL: return wput(w, "null", 4);
    case J_BOOL: return wput(w, v->u.b ? "true" : "false", v->u.b ? 4 : 5);
    case J_NUM:
        /* Preserve integer formatting when the value is integral */
        if (v->u.num == (double)(long long)v->u.num)
            snprintf(numbuf, sizeof(numbuf), "%lld", (long long)v->u.num);
        else
            snprintf(numbuf, sizeof(numbuf), "%.17g", v->u.num);
        return wput(w, numbuf, strlen(numbuf));
    case J_STR: return wstr(w, v->u.str.s, v->u.str.n);
    case J_ARR:
        if (wch(w, '[') < 0) return -1;
        for (i = 0; i < v->u.arr.n; i++) {
            if (i && wch(w, ',') < 0) return -1;
            if (wval(w, v->u.arr.items[i]) < 0) return -1;
        }
        return wch(w, ']');
    case J_OBJ: {
        if (wch(w, '{') < 0) return -1;
        for (i = 0; i < v->u.obj.n; i++) {
            if (i && wch(w, ',') < 0) return -1;
            if (wstr(w, v->u.obj.pairs[i].key,
                     strlen(v->u.obj.pairs[i].key)) < 0) return -1;
            if (wch(w, ':') < 0) return -1;
            if (wval(w, v->u.obj.pairs[i].val) < 0) return -1;
        }
        return wch(w, '}');
    }
    }
    return -1;
}

/* ---------------- Category detection ---------------- */
static int strhas(const char *hay, const char *needle) {
    /* case-insensitive substring */
    size_t nl = strlen(needle);
    const char *p = hay;
    if (!nl) return 0;
    for (; *p; p++) {
        size_t i;
        for (i = 0; i < nl; i++) {
            if (tolower((unsigned char)p[i]) != needle[i]) break;
        }
        if (i == nl) return 1;
    }
    return 0;
}

/* Returns the category bit for an item name, or 0 if unknown.
 * DISABLED 2026-10-04: The backend now sends correct AvatarItemType values
 * (OutfitType enum: 0=Hat, 100=Shoulder, 101=Shirt, etc.). ORing inferred bits
 * corrupts these values (e.g., 101|512=613) and breaks the client's tab filters.
 * Returning 0 makes the fixer a no-op, passing backend values through unchanged. */
static long long category_bit_for_name(const char *name) {
    (void)name;
    return 0;
}

/* ---------------- Fixer ---------------- */

/* Name-ish keys the client uses for display names. */
static const char *name_keys[] = {
    "FriendlyName", "Name", "DisplayName", "Title", NULL
};

/* Type-ish keys holding the category bitmask. */
static const char *type_keys[] = {
    "AvatarItemType", "OutfitType", NULL
};

static const char *item_name(jval_t *obj) {
    int i;
    for (i = 0; name_keys[i]; i++) {
        jval_t *v = jobj_get(obj, name_keys[i]);
        if (v && v->type == J_STR && v->u.str.n > 0)
            return v->u.str.s;
    }
    return NULL;
}

static long fix_count = 0;

/* Recursively walk; fix any object that has a type field, using the
 * nearest enclosing item name (objects without their own name inherit
 * the name from their parent, so nested AvatarItemInfo blocks get fixed
 * too). */
static void fix_walk_named(jval_t *v, const char *inherited_name) {
    size_t i;
    if (!v) return;
    if (v->type == J_OBJ) {
        const char *nm = item_name(v);
        const char *eff = nm ? nm : inherited_name;
        long long bit = eff ? category_bit_for_name(eff) : 0;
        if (bit) {
            int k;
            for (k = 0; type_keys[k]; k++) {
                jval_t *t = jobj_get(v, type_keys[k]);
                if (t && t->type == J_NUM) {
                    long long cur = (long long)t->u.num;
                    long long nw = cur | bit;
                    if (nw != cur) {
                        t->u.num = (double)nw;
                        fix_count++;
                    }
                }
            }
        }
        for (i = 0; i < v->u.obj.n; i++)
            fix_walk_named(v->u.obj.pairs[i].val, eff);
    } else if (v->type == J_ARR) {
        for (i = 0; i < v->u.arr.n; i++)
            fix_walk_named(v->u.arr.items[i], inherited_name);
    }
}

static void fix_walk(jval_t *v) {
    fix_walk_named(v, NULL);
}

long jsonfix_store_response(const char *in, size_t in_len,
                            char **out, size_t *out_len) {
    parser_t p;
    jval_t *root;
    writer_t w;
    long n;

    *out = NULL;
    if (out_len) *out_len = 0;

    p.p = in;
    p.end = in + in_len;
    root = pvalue(&p);
    if (!root) {
        /* Not valid JSON: return an unchanged copy. */
        char *cpy = (char *)malloc(in_len + 1);
        if (!cpy) return -1;
        memcpy(cpy, in, in_len);
        cpy[in_len] = '\0';
        *out = cpy;
        if (out_len) *out_len = in_len;
        return -1;
    }
    /* Trailing garbage check: allow only whitespace after the value. */
    pskip(&p);
    if (p.p != p.end) {
        jfree(root);
        char *cpy = (char *)malloc(in_len + 1);
        if (!cpy) return -1;
        memcpy(cpy, in, in_len);
        cpy[in_len] = '\0';
        *out = cpy;
        if (out_len) *out_len = in_len;
        return -1;
    }

    fix_count = 0;
    fix_walk(root);

    memset(&w, 0, sizeof(w));
    if (wval(&w, root) < 0) {
        jfree(root);
        free(w.buf);
        return -1;
    }
    jfree(root);
    if (wput(&w, "", 1) < 0) { free(w.buf); return -1; } /* NUL */
    n = fix_count;
    *out = w.buf;
    if (out_len) *out_len = w.n - 1;
    return n;
}
