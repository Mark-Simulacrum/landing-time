set title "Pull Request merge times for rust-lang/rust after last approval\n2 week intervals"
set ylabel "Time to merge (days)"
set xlabel "Date PR Opened"
set xdata time
set timefmt "%Y-%m-%d"
set format x "%m/%d/%y"
set datafile separator ","
set grid
# set samples 1000
datafile = 'merge_delay.csv'
coeff = 24
plot \
    datafile using 1:($2/coeff) smooth mcsplines title "85th", \
    datafile using 1:($3/coeff) smooth mcsplines title "90th", \
    datafile using 1:($4/coeff) smooth mcsplines title "95th", \
    datafile using 1:($5/coeff) smooth mcsplines title "98th", \
    # datafile using 1:($6/coeff) smooth mcsplines title "99th", \
    datafile using 1:($7/coeff) with lines title "mean", \
    # datafile using 1:($8/coeff) with lines title "max"
