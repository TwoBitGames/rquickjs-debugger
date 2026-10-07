(() => {
    const className = (v) => {
        try {
            const proto = Object.getPrototypeOf(v);
            if (proto === null) return "Object";
            const ctor = proto.constructor;
            if (ctor && typeof ctor.name === "string" && ctor.name) return ctor.name;
        } catch (_) {
        }
        return typeof v === "function" ? "Function" : "Object";
    };

    const describeNumber = (v) => {
        if (Number.isNaN(v)) return {type: "number", unserializableValue: "NaN", description: "NaN"};
        if (v === Infinity) return {type: "number", unserializableValue: "Infinity", description: "Infinity"};
        if (v === -Infinity) return {type: "number", unserializableValue: "-Infinity", description: "-Infinity"};
        if (Object.is(v, -0)) return {type: "number", unserializableValue: "-0", description: "-0"};
        return {type: "number", value: v, description: String(v)};
    };

    const describeFunction = (v) => {
        let source = "";
        try {
            source = Function.prototype.toString.call(v);
        } catch (_) {
        }
        return {
            type: "function",
            className: "Function",
            description: source || `function ${v.name || ""}() { [native code] }`,
        };
    };

    const describeObject = (v) => {
        const name = className(v);
        const d = {type: "object", className: name, description: name};
        if (Array.isArray(v)) {
            d.subtype = "array";
            d.description = `${name}(${v.length})`;
        } else if (v instanceof Error) {
            d.subtype = "error";
            const head = `${v.name}: ${v.message}`;
            d.description = typeof v.stack === "string" && v.stack ? `${head}\n${v.stack}`.trim() : head;
        } else if (v instanceof Date) {
            d.subtype = "date";
            d.description = Number.isNaN(v.getTime()) ? "Invalid Date" : v.toISOString();
        } else if (v instanceof RegExp) {
            d.subtype = "regexp";
            d.description = String(v);
        } else if (v instanceof Map) {
            d.subtype = "map";
            d.description = `Map(${v.size})`;
        } else if (v instanceof Set) {
            d.subtype = "set";
            d.description = `Set(${v.size})`;
        } else if (v instanceof WeakMap) {
            d.subtype = "weakmap";
        } else if (v instanceof WeakSet) {
            d.subtype = "weakset";
        } else if (v instanceof Promise) {
            d.subtype = "promise";
        } else if (ArrayBuffer.isView(v)) {
            d.subtype = v instanceof DataView ? "dataview" : "typedarray";
            d.description = `${name}(${v.length ?? v.byteLength})`;
        } else if (v instanceof ArrayBuffer) {
            d.subtype = "arraybuffer";
            d.description = `ArrayBuffer(${v.byteLength})`;
        } else if (name.endsWith("Iterator")) {
            d.subtype = "iterator";
        }
        return d;
    };

    const describe = (v) => {
        try {
            switch (typeof v) {
                case "undefined":
                    return {type: "undefined"};
                case "boolean":
                    return {type: "boolean", value: v};
                case "string":
                    return {type: "string", value: v};
                case "number":
                    return describeNumber(v);
                case "bigint":
                    return {type: "bigint", unserializableValue: `${v}n`, description: `${v}n`};
                case "symbol":
                    return {type: "symbol", description: String(v)};
                case "function":
                    return describeFunction(v);
                default:
                    if (v === null) return {type: "object", subtype: "null", value: null};
                    return describeObject(v);
            }
        } catch (e) {
            return {type: "object", className: "Object", description: `<unreadable: ${e && e.message}>`};
        }
    };

    const previewValue = (v) => {
        const d = describe(v);
        const out = {type: d.type};
        if (d.subtype) out.subtype = d.subtype;
        if (d.type === "object") out.value = d.subtype === "null" ? "null" : d.description;
        else if (d.type === "string") out.value = v;
        else if (d.type === "function") out.value = "";
        else out.value = d.description ?? String(d.value);
        return out;
    };

    const PREVIEW_LIMIT = 5;

    const previewEntry = (x) => {
        const d = describe(x);
        return {
            type: d.type,
            subtype: d.subtype,
            description: d.description ?? String(d.value),
            overflow: false,
            properties: [],
        };
    };

    const preview = (v, d) => {
        const p = {type: d.type, description: d.description, overflow: false, properties: []};
        if (d.subtype) p.subtype = d.subtype;
        let keys = [];
        try {
            keys = Object.keys(v);
        } catch (_) {

        }
        for (const k of keys) {
            if (p.properties.length >= PREVIEW_LIMIT) {
                p.overflow = true;
                break;
            }
            let desc;
            try {
                desc = Object.getOwnPropertyDescriptor(v, k);
            } catch (_) {
                continue;
            }
            if (!desc) continue;
            if ("value" in desc) p.properties.push({name: k, ...previewValue(desc.value)});
            else p.properties.push({name: k, type: "accessor"});
        }
        if (d.subtype === "map" || d.subtype === "set") {
            p.entries = [];
            let n = 0;
            for (const e of v) {
                if (n++ >= PREVIEW_LIMIT) {
                    p.overflow = true;
                    break;
                }
                p.entries.push(
                    d.subtype === "map" ? {
                        key: previewEntry(e[0]),
                        value: previewEntry(e[1])
                    } : {value: previewEntry(e)},
                );
            }
        }
        return p;
    };

    const PROPERTY_LIMIT = 2000;

    const props = (v, accessorsOnly) => {
        const meta = [];
        const values = [];
        const getters = [];
        const setters = [];
        let names = [];
        try {
            names = Reflect.ownKeys(v);
        } catch (_) {
        }
        for (const k of names) {
            if (meta.length >= PROPERTY_LIMIT) break;
            let desc;
            try {
                desc = Object.getOwnPropertyDescriptor(v, k);
            } catch (_) {
                continue;
            }
            if (!desc) continue;
            const accessor = !("value" in desc);
            if (accessorsOnly && !accessor) continue;
            meta.push({
                name: typeof k === "symbol" ? String(k) : k,
                enumerable: !!desc.enumerable,
                writable: !!desc.writable,
                configurable: !!desc.configurable,
                accessor,
            });
            values.push(accessor ? undefined : desc.value);
            getters.push(desc.get);
            setters.push(desc.set);
        }
        let proto = null;
        try {
            proto = Object.getPrototypeOf(v);
        } catch (_) {
        }
        return [JSON.stringify(meta), values, getters, setters, proto];
    };

    return {
        describe: (v, withPreview) => {
            const d = describe(v);
            if (withPreview && d.type === "object" && d.subtype !== "null") {
                try {
                    d.preview = preview(v, d);
                } catch (_) {

                }
            }
            return JSON.stringify(d);
        },
        props,
        callOn: (fn, thisArg, args) => Reflect.apply(fn, thisArg, args),
        matchUrl: (source, url) => new RegExp(source).test(url),
        checkRegex: (source) => {
            new RegExp(source);
        },
        toJson: (v) => JSON.stringify(v),
    };
})()
