// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! State-machine XML parser for IEC 62325 market documents using `quick-xml`.

use quick_xml::events::Event;
use quick_xml::reader::Reader;
use surge_network::network::market_data::{
    MarketData, MarketParticipant, MarketPeriod, MarketPoint, MarketTimeSeries,
};
use surge_network::network::time_utils::parse_iso8601;

use super::Iec62325Error;

/// Strip namespace prefix from an XML tag name (e.g. `ns1:mRID` -> `mRID`).
fn strip_ns(tag: &[u8]) -> &[u8] {
    match tag.iter().position(|&b| b == b':') {
        Some(i) => &tag[i + 1..],
        None => tag,
    }
}

/// Parser state machine for nested XML elements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Root,
    SenderParticipant,
    ReceiverParticipant,
    TimeSeries,
    Period,
    Point,
    TimeInterval,
}

pub(crate) fn parse_document(xml: &str) -> Result<MarketData, Iec62325Error> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut data = MarketData::default();
    let mut state = State::Root;

    // Current in-progress objects
    let mut cur_ts: Option<MarketTimeSeries> = None;
    let mut cur_period: Option<MarketPeriod> = None;
    let mut cur_point: Option<MarketPoint> = None;

    // Text accumulator for the current element
    let mut cur_tag: Option<String> = None;
    let mut text_buf = String::new();

    // Track sender vs receiver participant
    let mut sender_part = MarketParticipant::default();
    let mut receiver_part = MarketParticipant::default();
    let mut has_sender = false;
    let mut has_receiver = false;

    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Err(e) => return Err(Iec62325Error::Xml(format!("{e}"))),
            Ok(Event::Start(ref e)) => {
                let name = e.name();
                let local = strip_ns(name.as_ref());
                let tag = String::from_utf8_lossy(local).to_string();
                match tag.as_str() {
                    "TimeSeries" => {
                        state = State::TimeSeries;
                        cur_ts = Some(MarketTimeSeries::default());
                    }
                    "Period" if state == State::TimeSeries => {
                        state = State::Period;
                        cur_period = Some(MarketPeriod::default());
                    }
                    "Point" if state == State::Period => {
                        state = State::Point;
                        cur_point = Some(MarketPoint::default());
                    }
                    "timeInterval" if state == State::Period => {
                        state = State::TimeInterval;
                    }
                    "sender_MarketParticipant" => {
                        state = State::SenderParticipant;
                        sender_part = MarketParticipant::default();
                    }
                    "receiver_MarketParticipant" => {
                        state = State::ReceiverParticipant;
                        receiver_part = MarketParticipant::default();
                    }
                    _ => {}
                }
                cur_tag = Some(tag);
                text_buf.clear();
            }
            Ok(Event::End(ref e)) => {
                let name = e.name();
                let local = strip_ns(name.as_ref());
                let tag = String::from_utf8_lossy(local).to_string();
                let text = text_buf.trim().to_string();

                match tag.as_str() {
                    // Document-level fields
                    "mRID" if state == State::Root => {
                        data.document_mrid = Some(text);
                    }
                    "type" if state == State::Root => {
                        data.document_type = Some(text);
                    }

                    // Sender participant
                    "mRID" if state == State::SenderParticipant => {
                        sender_part.mrid = text;
                    }
                    "name" if state == State::SenderParticipant => {
                        sender_part.name = text;
                    }
                    "type" | "marketRole.type" if state == State::SenderParticipant => {
                        sender_part.role = Some(text);
                    }
                    "sender_MarketParticipant" => {
                        has_sender = true;
                        state = State::Root;
                    }

                    // Receiver participant
                    "mRID" if state == State::ReceiverParticipant => {
                        receiver_part.mrid = text;
                    }
                    "name" if state == State::ReceiverParticipant => {
                        receiver_part.name = text;
                    }
                    "type" | "marketRole.type" if state == State::ReceiverParticipant => {
                        receiver_part.role = Some(text);
                    }
                    "receiver_MarketParticipant" => {
                        has_receiver = true;
                        state = State::Root;
                    }

                    // TimeSeries fields
                    "mRID" if state == State::TimeSeries => {
                        if let Some(ref mut ts) = cur_ts {
                            ts.mrid = text;
                        }
                    }
                    "businessType" if state == State::TimeSeries => {
                        if let Some(ref mut ts) = cur_ts {
                            ts.business_type = Some(text);
                        }
                    }
                    "quantity_Measure_Unit.name"
                        if state == State::TimeSeries || state == State::Period =>
                    {
                        if let Some(ref mut ts) = cur_ts {
                            ts.quantity_unit = Some(text);
                        }
                    }
                    "curveType" if state == State::TimeSeries => {
                        if let Some(ref mut ts) = cur_ts {
                            ts.curve_type = Some(text);
                        }
                    }
                    "currency_Unit.name" if state == State::TimeSeries => {
                        if let Some(ref mut ts) = cur_ts {
                            ts.currency = Some(text);
                        }
                    }

                    // Domain references (handle dotted names via cur_tag)
                    "TimeSeries" => {
                        if let Some(ts) = cur_ts.take() {
                            data.time_series.push(ts);
                        }
                        state = State::Root;
                    }

                    // TimeInterval fields
                    "start" if state == State::TimeInterval => {
                        if let Some(ref mut p) = cur_period {
                            p.start = parse_iso8601(&text);
                        }
                    }
                    "end" if state == State::TimeInterval => {
                        if let Some(ref mut p) = cur_period {
                            p.end = parse_iso8601(&text);
                        }
                    }
                    "timeInterval" => {
                        state = State::Period;
                    }

                    // Period fields
                    "resolution" if state == State::Period => {
                        if let Some(ref mut p) = cur_period {
                            p.resolution = Some(text);
                        }
                    }
                    "Period" => {
                        if let Some(period) = cur_period.take()
                            && let Some(ref mut ts) = cur_ts
                        {
                            ts.periods.push(period);
                        }
                        state = State::TimeSeries;
                    }

                    // Point fields
                    "position" if state == State::Point => {
                        if let Some(ref mut pt) = cur_point {
                            pt.position = text.parse().unwrap_or(0);
                        }
                    }
                    "quantity" if state == State::Point => {
                        if let Some(ref mut pt) = cur_point {
                            pt.quantity = text.parse().ok();
                        }
                    }
                    "Point" => {
                        if let Some(point) = cur_point.take()
                            && let Some(ref mut p) = cur_period
                        {
                            p.points.push(point);
                        }
                        state = State::Period;
                    }

                    _ => {
                        // Handle dotted element names via the saved cur_tag
                        if let Some(ref saved) = cur_tag {
                            match saved.as_str() {
                                "in_Domain.mRID" if state == State::TimeSeries => {
                                    if let Some(ref mut ts) = cur_ts {
                                        ts.in_domain = Some(text);
                                    }
                                }
                                "out_Domain.mRID" if state == State::TimeSeries => {
                                    if let Some(ref mut ts) = cur_ts {
                                        ts.out_domain = Some(text);
                                    }
                                }
                                "registeredResource.mRID" if state == State::TimeSeries => {
                                    if let Some(ref mut ts) = cur_ts {
                                        ts.registered_resource = Some(text);
                                    }
                                }
                                "price.amount" if state == State::Point => {
                                    if let Some(ref mut pt) = cur_point {
                                        pt.price = text.parse().ok();
                                    }
                                }
                                "secondaryQuantity" if state == State::Point => {
                                    if let Some(ref mut pt) = cur_point {
                                        pt.secondary_quantity = text.parse().ok();
                                    }
                                }
                                "sender_MarketParticipant.mRID" if state == State::Root => {
                                    sender_part.mrid = text;
                                    has_sender = true;
                                }
                                "sender_MarketParticipant.marketRole.type"
                                    if state == State::Root =>
                                {
                                    sender_part.role = Some(text);
                                    has_sender = true;
                                }
                                "receiver_MarketParticipant.mRID" if state == State::Root => {
                                    receiver_part.mrid = text;
                                    has_receiver = true;
                                }
                                "receiver_MarketParticipant.marketRole.type"
                                    if state == State::Root =>
                                {
                                    receiver_part.role = Some(text);
                                    has_receiver = true;
                                }
                                _ => {}
                            }
                        }
                    }
                }
                cur_tag = None;
                text_buf.clear();
            }
            Ok(Event::Text(ref e)) => {
                text_buf.push_str(&e.unescape().unwrap_or_default());
            }
            Ok(Event::Empty(ref e)) => {
                // Self-closing tags — nothing to do for our structure.
                let _ = e;
            }
            _ => {}
        }
    }

    if has_sender {
        data.sender = Some(sender_part);
    }
    if has_receiver {
        data.receiver = Some(receiver_part);
    }

    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE_DOC: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<MarketDocument xmlns="urn:iec62325.351:tc57wg16:451-6:balancingdocument:3:0">
  <mRID>doc-001</mRID>
  <type>A25</type>
  <sender_MarketParticipant.mRID>10X1001A1001A123</sender_MarketParticipant.mRID>
  <sender_MarketParticipant.marketRole.type>A04</sender_MarketParticipant.marketRole.type>
  <receiver_MarketParticipant.mRID>10X1001A1001B456</receiver_MarketParticipant.mRID>
  <receiver_MarketParticipant.marketRole.type>A08</receiver_MarketParticipant.marketRole.type>
  <TimeSeries>
    <mRID>ts-001</mRID>
    <businessType>A01</businessType>
    <in_Domain.mRID>10YDE-VE-------2</in_Domain.mRID>
    <out_Domain.mRID>10YFR-RTE------C</out_Domain.mRID>
    <quantity_Measure_Unit.name>MAW</quantity_Measure_Unit.name>
    <curveType>A01</curveType>
    <Period>
      <timeInterval>
        <start>2024-01-15T00:00Z</start>
        <end>2024-01-16T00:00Z</end>
      </timeInterval>
      <resolution>PT60M</resolution>
      <Point>
        <position>1</position>
        <quantity>1500.0</quantity>
        <price.amount>45.20</price.amount>
      </Point>
      <Point>
        <position>2</position>
        <quantity>1450.0</quantity>
        <price.amount>42.80</price.amount>
      </Point>
    </Period>
  </TimeSeries>
</MarketDocument>"#;

    #[test]
    fn test_parse_simple_document() {
        let data = parse_document(SIMPLE_DOC).unwrap();
        assert_eq!(data.document_mrid.as_deref(), Some("doc-001"));
        assert_eq!(data.document_type.as_deref(), Some("A25"));
        assert_eq!(data.time_series.len(), 1);

        let ts = &data.time_series[0];
        assert_eq!(ts.mrid, "ts-001");
        assert_eq!(ts.business_type.as_deref(), Some("A01"));
        assert_eq!(ts.in_domain.as_deref(), Some("10YDE-VE-------2"));
        assert_eq!(ts.out_domain.as_deref(), Some("10YFR-RTE------C"));
        assert_eq!(ts.quantity_unit.as_deref(), Some("MAW"));
        assert_eq!(ts.curve_type.as_deref(), Some("A01"));

        assert_eq!(ts.periods.len(), 1);
        let p = &ts.periods[0];
        assert_eq!(p.start.unwrap().to_rfc3339(), "2024-01-15T00:00:00+00:00");
        assert_eq!(p.end.unwrap().to_rfc3339(), "2024-01-16T00:00:00+00:00");
        assert_eq!(p.resolution.as_deref(), Some("PT60M"));
        assert_eq!(p.points.len(), 2);
    }

    #[test]
    fn test_parse_points_quantity_and_price() {
        let data = parse_document(SIMPLE_DOC).unwrap();
        let pts = &data.time_series[0].periods[0].points;

        assert_eq!(pts[0].position, 1);
        assert!((pts[0].quantity.unwrap() - 1500.0).abs() < 1e-9);
        assert!((pts[0].price.unwrap() - 45.20).abs() < 1e-9);

        assert_eq!(pts[1].position, 2);
        assert!((pts[1].quantity.unwrap() - 1450.0).abs() < 1e-9);
        assert!((pts[1].price.unwrap() - 42.80).abs() < 1e-9);
    }

    #[test]
    fn test_parse_sender_receiver() {
        let data = parse_document(SIMPLE_DOC).unwrap();

        let sender = data.sender.as_ref().unwrap();
        assert_eq!(sender.mrid, "10X1001A1001A123");
        assert_eq!(sender.role.as_deref(), Some("A04"));

        let receiver = data.receiver.as_ref().unwrap();
        assert_eq!(receiver.mrid, "10X1001A1001B456");
        assert_eq!(receiver.role.as_deref(), Some("A08"));
    }

    #[test]
    fn test_parse_multiple_time_series() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<MarketDocument>
  <mRID>doc-multi</mRID>
  <TimeSeries>
    <mRID>ts-A</mRID>
    <businessType>A01</businessType>
    <Period>
      <timeInterval><start>2024-01-01T00:00Z</start><end>2024-01-02T00:00Z</end></timeInterval>
      <resolution>PT15M</resolution>
      <Point><position>1</position><quantity>100.0</quantity></Point>
    </Period>
  </TimeSeries>
  <TimeSeries>
    <mRID>ts-B</mRID>
    <businessType>A04</businessType>
    <Period>
      <timeInterval><start>2024-02-01T00:00Z</start><end>2024-02-02T00:00Z</end></timeInterval>
      <resolution>PT60M</resolution>
      <Point><position>1</position><quantity>200.0</quantity></Point>
      <Point><position>2</position><quantity>210.0</quantity></Point>
      <Point><position>3</position><quantity>190.0</quantity></Point>
    </Period>
  </TimeSeries>
</MarketDocument>"#;

        let data = parse_document(xml).unwrap();
        assert_eq!(data.time_series.len(), 2);
        assert_eq!(data.time_series[0].mrid, "ts-A");
        assert_eq!(data.time_series[0].periods[0].points.len(), 1);
        assert_eq!(data.time_series[1].mrid, "ts-B");
        assert_eq!(data.time_series[1].business_type.as_deref(), Some("A04"));
        assert_eq!(data.time_series[1].periods[0].points.len(), 3);
    }

    #[test]
    fn test_parse_missing_optional_fields() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<MarketDocument>
  <TimeSeries>
    <mRID>ts-sparse</mRID>
    <Period>
      <timeInterval><start>2024-03-01T00:00Z</start></timeInterval>
      <Point><position>1</position></Point>
    </Period>
  </TimeSeries>
</MarketDocument>"#;

        let data = parse_document(xml).unwrap();
        assert!(data.document_mrid.is_none());
        assert!(data.document_type.is_none());
        assert!(data.sender.is_none());
        assert!(data.receiver.is_none());

        let ts = &data.time_series[0];
        assert!(ts.business_type.is_none());
        assert!(ts.in_domain.is_none());
        assert!(ts.out_domain.is_none());
        assert!(ts.quantity_unit.is_none());
        assert!(ts.currency.is_none());
        assert!(ts.curve_type.is_none());

        let p = &ts.periods[0];
        assert!(p.end.is_none());
        assert!(p.resolution.is_none());

        let pt = &p.points[0];
        assert_eq!(pt.position, 1);
        assert!(pt.quantity.is_none());
        assert!(pt.price.is_none());
        assert!(pt.secondary_quantity.is_none());
    }

    #[test]
    fn test_parse_empty_document() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<MarketDocument>
</MarketDocument>"#;

        let data = parse_document(xml).unwrap();
        assert!(data.is_empty());
        assert!(data.time_series.is_empty());
        assert!(data.sender.is_none());
        assert!(data.receiver.is_none());
    }

    #[test]
    fn test_parse_namespaced_elements() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ns1:MarketDocument xmlns:ns1="urn:iec62325.351:tc57wg16:451-6:balancingdocument:3:0">
  <ns1:mRID>doc-ns</ns1:mRID>
  <ns1:type>A25</ns1:type>
  <ns1:TimeSeries>
    <ns1:mRID>ts-ns</ns1:mRID>
    <ns1:Period>
      <ns1:timeInterval><ns1:start>2024-06-01T00:00Z</ns1:start><ns1:end>2024-06-02T00:00Z</ns1:end></ns1:timeInterval>
      <ns1:resolution>PT60M</ns1:resolution>
      <ns1:Point><ns1:position>1</ns1:position><ns1:quantity>500.0</ns1:quantity></ns1:Point>
    </ns1:Period>
  </ns1:TimeSeries>
</ns1:MarketDocument>"#;

        let data = parse_document(xml).unwrap();
        assert_eq!(data.document_mrid.as_deref(), Some("doc-ns"));
        assert_eq!(data.document_type.as_deref(), Some("A25"));
        assert_eq!(data.time_series.len(), 1);
        assert_eq!(data.time_series[0].mrid, "ts-ns");
        let pt = &data.time_series[0].periods[0].points[0];
        assert!((pt.quantity.unwrap() - 500.0).abs() < 1e-9);
    }

    #[test]
    fn test_parse_nested_sender_receiver_elements() {
        // Some documents use nested elements instead of dotted names
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<MarketDocument>
  <mRID>doc-nested</mRID>
  <sender_MarketParticipant>
    <mRID>SENDER-001</mRID>
    <name>Sender Corp</name>
    <marketRole.type>A04</marketRole.type>
  </sender_MarketParticipant>
  <receiver_MarketParticipant>
    <mRID>RECV-001</mRID>
    <name>Receiver Corp</name>
    <marketRole.type>A08</marketRole.type>
  </receiver_MarketParticipant>
</MarketDocument>"#;

        let data = parse_document(xml).unwrap();
        let sender = data.sender.as_ref().unwrap();
        assert_eq!(sender.mrid, "SENDER-001");
        assert_eq!(sender.name, "Sender Corp");
        assert_eq!(sender.role.as_deref(), Some("A04"));

        let receiver = data.receiver.as_ref().unwrap();
        assert_eq!(receiver.mrid, "RECV-001");
        assert_eq!(receiver.name, "Receiver Corp");
        assert_eq!(receiver.role.as_deref(), Some("A08"));
    }
}
