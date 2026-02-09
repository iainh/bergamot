use quick_xml::Reader;
use quick_xml::Writer;
use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use std::io::Cursor;

pub fn parse_method_call(xml: &str) -> Result<(String, serde_json::Value), String> {
    let mut reader = Reader::from_str(xml);
    let mut method_name = String::new();
    let params: Vec<serde_json::Value> = Vec::new();
    let mut buf = Vec::new();
    let mut inside_method_name = false;
    let mut inside_params = false;
    let mut depth_stack: Vec<String> = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                depth_stack.push(tag.clone());
                if tag == "methodName" {
                    inside_method_name = true;
                } else if tag == "params" {
                    inside_params = true;
                }
            }
            Ok(Event::End(ref e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag == "methodName" {
                    inside_method_name = false;
                } else if tag == "params" {
                    inside_params = false;
                }
                depth_stack.pop();
            }
            Ok(Event::Text(ref e)) => {
                if inside_method_name {
                    method_name = e.unescape().map_err(|e| e.to_string())?.trim().to_string();
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("XML parse error: {e}")),
            _ => {}
        }
        buf.clear();
    }

    if inside_params || !params.is_empty() {
        // Re-parse for params
    }

    // Re-parse for param values
    let params = parse_params(xml)?;

    if method_name.is_empty() {
        return Err("missing methodName".to_string());
    }

    Ok((method_name, serde_json::json!(params)))
}

fn parse_params(xml: &str) -> Result<Vec<serde_json::Value>, String> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut params = Vec::new();
    let mut in_params = false;
    let mut in_param = false;
    let mut value_depth = 0;
    let mut value_xml = String::new();
    let mut capturing = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag == "params" {
                    in_params = true;
                } else if tag == "param" && in_params {
                    in_param = true;
                } else if tag == "value" && in_param && !capturing {
                    capturing = true;
                    value_depth = 0;
                    value_xml.clear();
                } else if capturing {
                    value_depth += 1;
                    value_xml.push_str(&format!("<{tag}>"));
                }
            }
            Ok(Event::End(ref e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag == "value" && capturing && value_depth == 0 {
                    capturing = false;
                    let val = parse_value(&value_xml)?;
                    params.push(val);
                    value_xml.clear();
                } else if capturing {
                    value_depth -= 1;
                    value_xml.push_str(&format!("</{tag}>"));
                } else if tag == "param" {
                    in_param = false;
                } else if tag == "params" {
                    in_params = false;
                }
            }
            Ok(Event::Text(ref e)) => {
                if capturing {
                    let text = e.unescape().map_err(|e| e.to_string())?;
                    value_xml.push_str(&text);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("XML parse error: {e}")),
            _ => {}
        }
        buf.clear();
    }

    Ok(params)
}

fn parse_value(xml: &str) -> Result<serde_json::Value, String> {
    let trimmed = xml.trim();
    if trimmed.is_empty() {
        return Ok(serde_json::Value::String(String::new()));
    }

    if trimmed.starts_with("<string>") {
        let inner = strip_tag(trimmed, "string");
        return Ok(serde_json::Value::String(inner));
    }
    if trimmed.starts_with("<i4>") {
        let inner = strip_tag(trimmed, "i4");
        let n: i64 = inner
            .parse()
            .map_err(|e: std::num::ParseIntError| e.to_string())?;
        return Ok(serde_json::json!(n));
    }
    if trimmed.starts_with("<int>") {
        let inner = strip_tag(trimmed, "int");
        let n: i64 = inner
            .parse()
            .map_err(|e: std::num::ParseIntError| e.to_string())?;
        return Ok(serde_json::json!(n));
    }
    if trimmed.starts_with("<boolean>") {
        let inner = strip_tag(trimmed, "boolean");
        return Ok(serde_json::json!(inner == "1"));
    }
    if trimmed.starts_with("<double>") {
        let inner = strip_tag(trimmed, "double");
        let n: f64 = inner
            .parse()
            .map_err(|e: std::num::ParseFloatError| e.to_string())?;
        return Ok(serde_json::json!(n));
    }
    if trimmed.starts_with("<base64>") {
        let inner = strip_tag(trimmed, "base64");
        return Ok(serde_json::Value::String(inner));
    }
    if trimmed.starts_with("<array>") {
        return parse_array_value(trimmed);
    }
    if trimmed.starts_with("<struct>") {
        return parse_struct_value(trimmed);
    }

    // Plain text with no type tag is a string
    Ok(serde_json::Value::String(trimmed.to_string()))
}

fn strip_tag(xml: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    xml.strip_prefix(&open)
        .and_then(|s| s.strip_suffix(&close))
        .unwrap_or(xml)
        .trim()
        .to_string()
}

fn parse_array_value(xml: &str) -> Result<serde_json::Value, String> {
    let inner = strip_tag(xml, "array");
    let data_inner = strip_tag(&inner, "data");

    let mut values = Vec::new();
    let mut reader = Reader::from_str(&data_inner);
    let mut buf = Vec::new();
    let mut value_depth = 0;
    let mut value_xml = String::new();
    let mut capturing = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag == "value" && !capturing {
                    capturing = true;
                    value_depth = 0;
                    value_xml.clear();
                } else if capturing {
                    value_depth += 1;
                    value_xml.push_str(&format!("<{tag}>"));
                }
            }
            Ok(Event::End(ref e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag == "value" && capturing && value_depth == 0 {
                    capturing = false;
                    values.push(parse_value(&value_xml)?);
                    value_xml.clear();
                } else if capturing {
                    value_depth -= 1;
                    value_xml.push_str(&format!("</{tag}>"));
                }
            }
            Ok(Event::Text(ref e)) => {
                if capturing {
                    let text = e.unescape().map_err(|e| e.to_string())?;
                    value_xml.push_str(&text);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("XML parse error: {e}")),
            _ => {}
        }
        buf.clear();
    }

    Ok(serde_json::json!(values))
}

fn parse_struct_value(xml: &str) -> Result<serde_json::Value, String> {
    let inner = strip_tag(xml, "struct");
    let mut map = serde_json::Map::new();
    let mut reader = Reader::from_str(&inner);
    let mut buf = Vec::new();
    let mut in_member = false;
    let mut in_name = false;
    let mut current_name = String::new();
    let mut value_depth = 0;
    let mut value_xml = String::new();
    let mut capturing_value = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag == "member" {
                    in_member = true;
                    current_name.clear();
                } else if tag == "name" && in_member {
                    in_name = true;
                } else if tag == "value" && in_member && !capturing_value {
                    capturing_value = true;
                    value_depth = 0;
                    value_xml.clear();
                } else if capturing_value {
                    value_depth += 1;
                    value_xml.push_str(&format!("<{tag}>"));
                }
            }
            Ok(Event::End(ref e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag == "name" {
                    in_name = false;
                } else if tag == "value" && capturing_value && value_depth == 0 {
                    capturing_value = false;
                    let val = parse_value(&value_xml)?;
                    map.insert(current_name.clone(), val);
                    value_xml.clear();
                } else if capturing_value {
                    value_depth -= 1;
                    value_xml.push_str(&format!("</{tag}>"));
                } else if tag == "member" {
                    in_member = false;
                }
            }
            Ok(Event::Text(ref e)) => {
                let text = e.unescape().map_err(|e| e.to_string())?;
                if in_name {
                    current_name = text.trim().to_string();
                } else if capturing_value {
                    value_xml.push_str(&text);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("XML parse error: {e}")),
            _ => {}
        }
        buf.clear();
    }

    Ok(serde_json::Value::Object(map))
}

pub fn json_to_xmlrpc_response(result: &serde_json::Value) -> String {
    let mut writer = Writer::new(Cursor::new(Vec::new()));

    writer
        .write_event(Event::Text(BytesText::new("<?xml version=\"1.0\"?>")))
        .ok();
    write_start(&mut writer, "methodResponse");
    write_start(&mut writer, "params");
    write_start(&mut writer, "param");
    write_json_value(&mut writer, result);
    write_end(&mut writer, "param");
    write_end(&mut writer, "params");
    write_end(&mut writer, "methodResponse");

    String::from_utf8(writer.into_inner().into_inner()).unwrap_or_default()
}

pub fn json_to_xmlrpc_fault(code: i32, message: &str) -> String {
    let mut writer = Writer::new(Cursor::new(Vec::new()));

    writer
        .write_event(Event::Text(BytesText::new("<?xml version=\"1.0\"?>")))
        .ok();
    write_start(&mut writer, "methodResponse");
    write_start(&mut writer, "fault");
    write_start(&mut writer, "value");
    write_start(&mut writer, "struct");

    write_start(&mut writer, "member");
    write_text_element(&mut writer, "name", "faultCode");
    write_start(&mut writer, "value");
    write_text_element(&mut writer, "int", &code.to_string());
    write_end(&mut writer, "value");
    write_end(&mut writer, "member");

    write_start(&mut writer, "member");
    write_text_element(&mut writer, "name", "faultString");
    write_start(&mut writer, "value");
    write_text_element(&mut writer, "string", message);
    write_end(&mut writer, "value");
    write_end(&mut writer, "member");

    write_end(&mut writer, "struct");
    write_end(&mut writer, "value");
    write_end(&mut writer, "fault");
    write_end(&mut writer, "methodResponse");

    String::from_utf8(writer.into_inner().into_inner()).unwrap_or_default()
}

fn write_start(writer: &mut Writer<Cursor<Vec<u8>>>, tag: &str) {
    writer.write_event(Event::Start(BytesStart::new(tag))).ok();
}

fn write_end(writer: &mut Writer<Cursor<Vec<u8>>>, tag: &str) {
    writer.write_event(Event::End(BytesEnd::new(tag))).ok();
}

fn write_text_element(writer: &mut Writer<Cursor<Vec<u8>>>, tag: &str, text: &str) {
    write_start(writer, tag);
    writer.write_event(Event::Text(BytesText::new(text))).ok();
    write_end(writer, tag);
}

fn write_json_value(writer: &mut Writer<Cursor<Vec<u8>>>, val: &serde_json::Value) {
    write_start(writer, "value");
    match val {
        serde_json::Value::Null => {
            write_text_element(writer, "string", "");
        }
        serde_json::Value::Bool(b) => {
            write_text_element(writer, "boolean", if *b { "1" } else { "0" });
        }
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                write_text_element(writer, "int", &i.to_string());
            } else if let Some(f) = n.as_f64() {
                write_text_element(writer, "double", &f.to_string());
            }
        }
        serde_json::Value::String(s) => {
            write_text_element(writer, "string", s);
        }
        serde_json::Value::Array(arr) => {
            write_start(writer, "array");
            write_start(writer, "data");
            for item in arr {
                write_json_value(writer, item);
            }
            write_end(writer, "data");
            write_end(writer, "array");
        }
        serde_json::Value::Object(obj) => {
            write_start(writer, "struct");
            for (key, value) in obj {
                write_start(writer, "member");
                write_text_element(writer, "name", key);
                write_json_value(writer, value);
                write_end(writer, "member");
            }
            write_end(writer, "struct");
        }
    }
    write_end(writer, "value");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_method_call() {
        let xml = r#"<?xml version="1.0"?>
        <methodCall>
            <methodName>status</methodName>
            <params></params>
        </methodCall>"#;
        let (method, params) = parse_method_call(xml).unwrap();
        assert_eq!(method, "status");
        assert_eq!(params, serde_json::json!([]));
    }

    #[test]
    fn parse_method_call_with_string_param() {
        let xml = r#"<?xml version="1.0"?>
        <methodCall>
            <methodName>append</methodName>
            <params>
                <param><value><string>/tmp/test.nzb</string></value></param>
            </params>
        </methodCall>"#;
        let (method, params) = parse_method_call(xml).unwrap();
        assert_eq!(method, "append");
        assert_eq!(params, serde_json::json!(["/tmp/test.nzb"]));
    }

    #[test]
    fn parse_method_call_with_int_param() {
        let xml = r#"<?xml version="1.0"?>
        <methodCall>
            <methodName>rate</methodName>
            <params>
                <param><value><i4>500</i4></value></param>
            </params>
        </methodCall>"#;
        let (method, params) = parse_method_call(xml).unwrap();
        assert_eq!(method, "rate");
        assert_eq!(params, serde_json::json!([500]));
    }

    #[test]
    fn parse_method_call_with_boolean_param() {
        let xml = r#"<?xml version="1.0"?>
        <methodCall>
            <methodName>history</methodName>
            <params>
                <param><value><boolean>0</boolean></value></param>
            </params>
        </methodCall>"#;
        let (method, params) = parse_method_call(xml).unwrap();
        assert_eq!(method, "history");
        assert_eq!(params, serde_json::json!([false]));
    }

    #[test]
    fn parse_method_call_no_params_tag() {
        let xml = r#"<?xml version="1.0"?>
        <methodCall>
            <methodName>version</methodName>
        </methodCall>"#;
        let (method, params) = parse_method_call(xml).unwrap();
        assert_eq!(method, "version");
        assert_eq!(params, serde_json::json!([]));
    }

    #[test]
    fn json_to_xmlrpc_response_string() {
        let result = serde_json::json!("0.1.0");
        let xml = json_to_xmlrpc_response(&result);
        assert!(xml.contains("<methodResponse>"));
        assert!(xml.contains("<string>0.1.0</string>"));
        assert!(xml.contains("</methodResponse>"));
    }

    #[test]
    fn json_to_xmlrpc_response_struct() {
        let result = serde_json::json!({"DownloadRate": 512});
        let xml = json_to_xmlrpc_response(&result);
        assert!(xml.contains("<struct>"));
        assert!(xml.contains("<name>DownloadRate</name>"));
        assert!(xml.contains("<int>512</int>"));
    }

    #[test]
    fn json_to_xmlrpc_fault_format() {
        let xml = json_to_xmlrpc_fault(-32601, "Method not found");
        assert!(xml.contains("<fault>"));
        assert!(xml.contains("<name>faultCode</name>"));
        assert!(xml.contains("<int>-32601</int>"));
        assert!(xml.contains("<name>faultString</name>"));
        assert!(xml.contains("<string>Method not found</string>"));
    }

    #[test]
    fn json_to_xmlrpc_response_array() {
        let result = serde_json::json!([1, "two", true]);
        let xml = json_to_xmlrpc_response(&result);
        assert!(xml.contains("<array>"));
        assert!(xml.contains("<int>1</int>"));
        assert!(xml.contains("<string>two</string>"));
        assert!(xml.contains("<boolean>1</boolean>"));
    }
}
