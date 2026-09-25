// Pure mapping logic for the "Appear Online To" presence slider fix.
//
// This file is intentionally free of Unity / BepInEx / Harmony dependencies so it
// can be unit-tested on any machine (see hidden_files/plugin-work/tests/).
//
// Background (verified by disassembly of GameAssembly.dll, client 2023-04-14):
//   - The presence enum JFAAGFOLICM is: Public=0, FriendsOnly=1,
//     FavoriteFriendsOnly=2, Offline=3.
//   - The Experience-page PRESENCE slider impl (SettingsModelController.
//     AppearOnlineToSliderImpl) maps slider position <-> stored enum with an
//     IDENTITY conversion in both directions (float -> (int) -> enum on write,
//     (float)enum on refresh).
//   - The display formatter (BaseAccountModel.FormatOnlineStatusVisibility) is
//     also identity: enum value N -> label N.
//
// The only layer not visible in the native binary is the slider's notch labels,
// and the observed symptom (selecting "All" stores/displays "No One") proves
// they run OPPOSITE to the enum: notch 0 = "No One", notch 1 = "Favorites",
// notch 2 = "Friends", notch 3 = "All". So the plugin must invert the mapping:
// stored enum = 3 - slider position (and vice versa on refresh), giving:
//   "No One" (pos 0) -> Offline(3), "Favorites" (pos 1) -> FavoriteFriendsOnly(2),
//   "Friends"  (pos 2) -> FriendsOnly(1),  "All" (pos 3) -> Public(0).
//
// If a future client build ever ships the labels in enum order, turn the
// [Presence] "Fix Appear Online To Mapping" knob off.
namespace RecNetPlugin.Patches;

public static class PresenceMapping
{
    /// <summary>Number of stops on the Appear-Online-To slider.</summary>
    public const int OptionCount = 4;

    /// <summary>
    /// Convert a slider notch position (0..3) to the presence enum value to store.
    /// Inverts the mapping because the notch labels run opposite to the enum order.
    /// Out-of-range positions are clamped to the nearest valid notch.
    /// </summary>
    public static int SliderPositionToPresenceValue(int position)
    {
        return (OptionCount - 1) - ClampPosition(position);
    }

    /// <summary>
    /// Convert a stored presence enum value (0..3) to the slider notch position
    /// that displays it. Inverse of <see cref="SliderPositionToPresenceValue"/>.
    /// Out-of-range values are clamped to the nearest valid notch.
    /// </summary>
    public static int PresenceValueToSliderPosition(int presenceValue)
    {
        return (OptionCount - 1) - ClampPosition(presenceValue);
    }

    /// <summary>
    /// Convert a raw float slider value to the presence enum value to store,
    /// mirroring the game's own float->int truncation but tolerant of float
    /// imprecision at notch boundaries (rounds to nearest notch first).
    /// </summary>
    public static int SliderFloatToPresenceValue(float sliderValue)
    {
        return SliderPositionToPresenceValue((int)System.MathF.Round(sliderValue));
    }

    private static int ClampPosition(int position)
    {
        if (position < 0) return 0;
        if (position >= OptionCount) return OptionCount - 1;
        return position;
    }
}
