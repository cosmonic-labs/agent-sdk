mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "converter",
    });
}

use bindings::exports::example::units::temperature::{
    ConvertArgs, ConvertResult, Error, Guest, Scale,
};

/// Every scale through kelvin.
fn to_kelvin(value: f64, scale: Scale) -> f64 {
    match scale {
        Scale::Celsius => value + 273.15,
        Scale::Fahrenheit => (value - 32.0) * 5.0 / 9.0 + 273.15,
        Scale::Kelvin => value,
    }
}

fn from_kelvin(kelvin: f64, scale: Scale) -> f64 {
    match scale {
        Scale::Celsius => kelvin - 273.15,
        Scale::Fahrenheit => (kelvin - 273.15) * 9.0 / 5.0 + 32.0,
        Scale::Kelvin => kelvin,
    }
}

struct Component;

impl Guest for Component {
    async fn convert(args: ConvertArgs) -> Result<ConvertResult, Error> {
        let kelvin = to_kelvin(args.value, args.from);
        if kelvin < 0.0 {
            return Err(Error::BelowAbsoluteZero(format!(
                "{:.2} below absolute zero",
                -kelvin
            )));
        }
        let value = (from_kelvin(kelvin, args.to) * 100.0).round() / 100.0;
        Ok(ConvertResult { value })
    }
}

bindings::export!(Component with_types_in bindings);
