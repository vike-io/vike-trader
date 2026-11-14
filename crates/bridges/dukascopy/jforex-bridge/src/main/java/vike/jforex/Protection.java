package vike.jforex;

import com.dukascopy.api.IMessage;
import com.dukascopy.api.IOrder;
import com.dukascopy.api.JFException;

/**
 * The two native protections JForex holds on an open (FILLED) order, and everything that tells them apart: the price an
 * order carries now, how it is changed, the message reason that answers the change and the reason that says the
 * protection FIRED. Both are a price set on a WHOLE order (it has no amount of its own), held and executed by the venue
 * with no client attached, and answered by {@code ORDER_CHANGED_OK} / {@code ORDER_CHANGED_REJECTED}; they differ only
 * in these five facts, so the stop-loss code in {@link StrategyBridge} is one path parameterised by this enum.
 */
enum Protection {
    /** A reduce-only {@code stop}: {@code IOrder.setStopLossPrice}. */
    STOP_LOSS("stop-loss", "stop", IMessage.Reason.ORDER_CHANGED_SL, IMessage.Reason.ORDER_CLOSED_BY_SL),
    /** A reduce-only {@code limit}: {@code IOrder.setTakeProfitPrice} (a limit that closes, never opens). */
    TAKE_PROFIT("take-profit", "take-profit", IMessage.Reason.ORDER_CHANGED_TP, IMessage.Reason.ORDER_CLOSED_BY_TP);

    /** What the venue calls it, for words ("stop-loss", "take-profit"). */
    final String word;
    /** What the node's order is called, for words ("stop", "take-profit"). */
    final String noun;
    /** The {@code ORDER_CHANGED_OK} reason that answers a change of this price. */
    final IMessage.Reason changed;
    /** The {@code ORDER_CLOSE_OK} reason that says this price was reached and the order closed. */
    final IMessage.Reason closedBy;

    Protection(String word, String noun, IMessage.Reason changed, IMessage.Reason closedBy) {
        this.word = word;
        this.noun = noun;
        this.changed = changed;
        this.closedBy = closedBy;
    }

    /** The price `order` carries now (0 or less = none). */
    double priceOn(IOrder order) {
        return this == STOP_LOSS ? order.getStopLossPrice() : order.getTakeProfitPrice();
    }

    /** Ask the venue to put `price` on `order` (0 or less takes it off). The answer arrives as a message. */
    void setOn(IOrder order, double price) throws JFException {
        if (this == STOP_LOSS) {
            order.setStopLossPrice(price);
        } else {
            order.setTakeProfitPrice(price);
        }
    }
}
