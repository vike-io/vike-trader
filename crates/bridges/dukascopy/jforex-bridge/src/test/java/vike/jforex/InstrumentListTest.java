package vike.jforex;

import com.dukascopy.api.Instrument;
import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * The {@code instruments} line over the SDK's own {@link Instrument} constants, with no login (a login fills the group,
 * the country, the description and the server's pip; here they are the SDK's predefined values): the shape the Rust
 * reader takes ({@code crates/bridges/dukascopy/src/instrument/grid.rs}'s {@code JforexInstrument}), sorted by the wire
 * spelling, and valid JSON for every instrument the SDK predefines.
 */
class InstrumentListTest {

    private static List<String> symbols(JsonArray rows) {
        List<String> out = new ArrayList<>();
        rows.forEach(r -> out.add(r.getAsJsonObject().get("symbol").getAsString()));
        return out;
    }

    @Test
    void aRowCarriesTheWireSpellingTheSdkNameTheClassAndTheGridSortedBySymbol() {
        JsonObject line = InstrumentList.envelope(
                List.of(Instrument.USDJPY, Instrument.EURUSD, Instrument.COPPERCMDUSD, Instrument.XAUUSD));
        assertEquals("instruments", line.get("kind").getAsString());
        JsonArray rows = line.getAsJsonArray("instruments");
        assertEquals(List.of("COPPER.CMD/USD", "EURUSD", "USDJPY", "XAUUSD"), symbols(rows));

        JsonObject eur = rows.get(1).getAsJsonObject();
        assertEquals("EUR/USD", eur.get("pair").getAsString());
        assertEquals("FOREX", eur.get("type").getAsString());
        assertEquals(0.0001, eur.get("pip_value").getAsDouble());
        assertEquals(4, eur.get("pip_scale").getAsInt());
        assertEquals(5, eur.get("tick_scale").getAsInt());

        JsonObject copper = rows.get(0).getAsJsonObject();
        assertEquals("COPPER.CMD/USD", copper.get("pair").getAsString());
        assertEquals("CFD", copper.get("type").getAsString());
        assertEquals(4, copper.get("tick_scale").getAsInt());

        JsonObject gold = rows.get(3).getAsJsonObject();
        assertEquals("METAL", gold.get("type").getAsString());
        assertEquals(3, gold.get("tick_scale").getAsInt());
        // the login's sizes are SDK class constants, so the line carries none: the `terms` line does
        assertFalse(gold.has("min") || gold.has("step") || gold.has("max"), gold.toString());
    }

    @Test
    void everyPredefinedInstrumentIsARowAndTheLineIsValidJson() {
        JsonObject line = InstrumentList.envelope(Arrays.asList(Instrument.values()));
        JsonArray rows = line.getAsJsonArray("instruments");
        assertEquals(Instrument.values().length, rows.size(), "no instrument left out");
        String text = line.toString();
        assertFalse(text.contains("NaN") || text.contains("Infinity"), "a figure that is no number is omitted");
        assertTrue(JsonParser.parseString(text).isJsonObject());
        for (var r : rows) {
            JsonObject row = r.getAsJsonObject();
            assertEquals(row.get("pip_scale").getAsInt() + 1, row.get("tick_scale").getAsInt(), row.toString());
            assertTrue(row.get("pip_value").getAsDouble() > 0, row.toString());
        }
    }
}
